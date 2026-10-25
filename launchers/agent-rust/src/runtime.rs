use crate::{
    attachments::Attachments,
    goals::{self, Goals},
    model::{self, now, preview},
    rpc::Rpc,
    store::Store,
    transcript::{HistoryLimit, InvalidTurnIdentity, Transcript},
};
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::{HashSet, VecDeque}, path::PathBuf, process::Stdio, sync::Arc};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::{mpsc, oneshot, watch},
    time::{timeout, Duration},
};
mod messages;
mod receipts;
mod message_retry;
mod recovery;
mod session_snapshot;

pub struct Request {
    pub method: String,
    pub body: Value,
    pub reply: oneshot::Sender<Result<Value>>,
}
#[derive(Clone)]
pub struct Handle {
    pub commands: mpsc::Sender<Request>,
    pub state: watch::Receiver<Arc<Value>>,
    shutdown: Arc<tokio::sync::Notify>,
    stopped: watch::Receiver<Option<bool>>,
}
impl Handle {
    pub async fn shutdown(&self) -> Result<()> {
        self.shutdown.notify_one();
        let mut stopped = self.stopped.clone();
        timeout(Duration::from_secs(7), async {
            while stopped.borrow_and_update().is_none() {
                stopped.changed().await.map_err(|_| anyhow!("runtime_shutdown_unverified"))?;
            }
            Ok::<_, anyhow::Error>(())
        }).await.map_err(|_| anyhow!("runtime_shutdown_unverified"))??;
        if *stopped.borrow() == Some(false) {
            bail!("private_state_write_failed");
        }
        Ok(())
    }
    pub async fn call(&self, method: &str, body: Value) -> Result<Value> {
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(Request {
                method: method.into(),
                body,
                reply: tx,
            })
            .await?;
        timeout(Duration::from_secs(65), rx)
            .await?
            .map_err(|_| anyhow!("runtime stopped"))?
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Record {
    operation: String,
    fingerprint: String,
    text: String,
    delivery: String,
    submission: Value,
    receipt: Value,
    notify: Value,
    #[serde(default)]
    notified: bool,
    #[serde(default)]
    images: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    node_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    source_messages: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    message_batch: Option<messages::Batch>,
    // Display-only boundary observed BEFORE a steer RPC. This is not provider
    // turn authority and never authorizes dispatch/reconciliation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    projection_after: Option<String>,
}
struct Runtime {
    claude:bool,
    snapshot_source: Option<(PathBuf, PathBuf)>,
    snapshot_read_slot: Arc<tokio::sync::Semaphore>,
    terminal_tasks: Value,
    rpc: Rpc,
    events: mpsc::Receiver<Value>,
    rpc_completed_turns: HashSet<String>,
    store: Store,
    thread: Value,
    models: Value,
    model_revision: u64,
    effort_revision: u64,
    ready: bool,
    turns: Transcript,
    history_start: usize,
    history_total: usize,
    history_nonce: String,
    last_history_page: Option<std::time::Instant>,
    // Sticky stop for history admission OR private journal failure.
    history_failed: bool,
    state_failed: bool,
    transport_failed: bool,
    last_disk_snapshot: Option<std::time::Instant>,
    disk_history_status: &'static str,
    provider_exit: &'static str,
    final_projection: Value,
    records: VecDeque<Record>,
    dispatch_blocked: bool,
    reconnect_attempts: u32,
    reconnect_due: std::time::Instant,
    recovering: bool,
    recovery_reads: u64,
    idle_proof: Option<std::time::Instant>,
    recovery_generation: u64,
    goals: Goals,
    // Last successfully published state; rollback is centralized for every
    // checkpoint caller, including provider events and notification acks.
    checkpoint: (VecDeque<Record>, Goals),
    goals_supported: bool,
    claude_goal_wait: Option<(String, std::time::Instant)>,
    claude_goal_ack: Option<Value>,
    attachments: Option<Arc<tokio::sync::Mutex<Attachments>>>,
    changed: watch::Sender<Arc<Value>>,
}
struct Notification {
    id: String,
    args: Vec<String>,
    payload: String,
}

// Cancellation must not detach auxiliary workers when the actor is stopped
// during an in-flight provider RPC or startup handshake.
struct AbortTask(tokio::task::JoinHandle<()>);
impl Drop for AbortTask {
    fn drop(&mut self) { self.0.abort(); }
}

#[derive(Clone)]
pub struct Config {
    pub claude:bool,
    pub claude_config_dir: Option<PathBuf>,
    pub cwd: PathBuf,
    pub private_root: PathBuf,
    pub codex: String,
    pub codex_args: Vec<String>,
    pub session: Option<String>,
    pub attachments: Option<Arc<tokio::sync::Mutex<Attachments>>>,
    pub capture_stderr: bool,
}
pub fn spawn(mut config: Config) -> Result<Handle> {
    let store = Store::open(&config.private_root, &config.cwd)?;
    let saved = store.load().map_err(|_| anyhow!("private_state_invalid"))?;
    if config.claude {
        if let Some(saved)=&saved {
            if saved["harness"]!="claude" {bail!("private_state_harness_mismatch");}
        } else if config.session.is_some() {bail!("claude_resume_requires_supported_history_checkpoint");}
    } else if saved.as_ref().is_some_and(|s|s["harness"]=="claude") {bail!("private_state_harness_mismatch");}
    // Refuse malformed durable dispatch state before starting a provider. Never
    // discard an invalid record: it may contain an uncertain dispatch intent.
    if let Some(saved) = &saved {
        decode_records(saved)?;
        Goals::load(&saved["goals"])?;
        if config.session.as_ref().is_some_and(|session| saved["threadId"].as_str() != Some(session.as_str())) {
            bail!("private_state_session_mismatch");
        }
    }
    let (rpc, mut child, events) = if config.claude {
        let session=config.session.clone().or_else(||saved.as_ref().and_then(|s|s["threadId"].as_str()).map(str::to_owned)).unwrap_or_else(||uuid::Uuid::new_v4().to_string());
        config.codex_args.extend([if saved.is_some(){"--resume"}else{"--session-id"}.into(),session.clone()]);
        Rpc::spawn_claude(&config.codex,&config.codex_args,&config.cwd,config.capture_stderr,session)?
    }else{Rpc::spawn(&config.codex, &config.codex_args, &config.cwd, config.capture_stderr)?};
    let (commands, mut requests) = mpsc::channel::<Request>(64);
    let shutdown = Arc::new(tokio::sync::Notify::new());
    let shutdown_requested = shutdown.clone();
    let (stopped_tx, stopped) = watch::channel(None);
    let (changed, state) = watch::channel(Arc::new(
        json!({"ready":false,"runtime":{"state":"starting","error":null},"thread":null,"queue":[],"modelOptions":[],"backgroundTasks":[],"goal":null}),
    ));
    let mut rt = Runtime {
        claude:config.claude,
        snapshot_source: config.claude_config_dir.clone().map(|root| (root,config.cwd.clone())),
        snapshot_read_slot: Arc::new(tokio::sync::Semaphore::new(1)),
        rpc,
        events,
        rpc_completed_turns: HashSet::new(),
        terminal_tasks: json!([]),
        store,
        thread: Value::Null,
        models: json!([]),
        model_revision: 0,
        effort_revision: 0,
        ready: false,
        turns: Transcript::default(),
        history_start: 0,
        history_total: 0,
        history_nonce: uuid::Uuid::new_v4().simple().to_string(),
        last_history_page: None,
        history_failed: false,
        state_failed: false,
        transport_failed: false,
        last_disk_snapshot: None,
        disk_history_status: "not_requested",
        provider_exit: "not_requested",
        final_projection: Value::Null,
        records: VecDeque::new(),
        dispatch_blocked: false,
        reconnect_attempts: 0,
        reconnect_due: std::time::Instant::now(),
        recovering: false,
        recovery_reads: 0,
        idle_proof: None,
        recovery_generation: 0,
        goals: Goals::default(),
        checkpoint: (VecDeque::new(), Goals::default()),
        goals_supported: false,
        claude_goal_wait: None,
        claude_goal_ack: None,
        attachments: config.attachments.clone(),
        changed,
    };
    tokio::spawn(async move {
        // Cancellation is safe only at await boundaries: journal publication
        // stays synchronous/atomic. Future async writes must finish outside
        // this cancellable region before an intent can reach the provider.
        tokio::select! {
        biased;
        _ = shutdown_requested.notified() => {},
        _ = async {
        let startup = rt.start(&config, saved).await;
        if let Err(error) = startup {
            eprintln!("Rust spike: Codex startup failed (no fresh-thread fallback)");
            let reason = if rt.state_failed {
                "private_state_write_failed; queue paused"
            } else if error.is::<HistoryLimit>() {
                rt.dispatch_blocked = true;
                "Codex thread exceeds launcher history limit; use a launcher supporting larger history; no fresh-thread fallback"
            } else if error.is::<InvalidTurnIdentity>() {
                rt.dispatch_blocked = true;
                "Invalid provider turn identity; no fresh-thread fallback"
            } else {
                "Codex startup failed"
            };
            rt.turns = Transcript::default();
            rt.thread = json!({"status":"error","lastError":reason,"turns":[]});
            rt.publish();
            rt.record_provider_stop(timeout(Duration::from_secs(5), child.kill()).await);
            return;
        }
        // One bounded sender, one bounded result, at most one in-flight send.
        // Network/CLI latency never occupies the runtime actor.
        let (notify_tx, mut notify_rx) = mpsc::channel::<Notification>(1);
        let (result_tx, mut result_rx) = mpsc::channel(1);
        let notification_worker = AbortTask(tokio::spawn(async move {
            while let Some(job) = notify_rx.recv().await {
                let success = message_command(&job.args, Some(&job.payload)).await.is_ok();
                if result_tx.send((job.id, success)).await.is_err() {
                    break;
                }
            }
        }));
        let mut notification_pending = false;
        // Read-only reconciliation stays outside the actor with one pending
        // read/result, so a slow read cannot block interrupts or grow a backlog.
        let (observe_tx, mut observe_rx) = mpsc::channel::<(u64, u64, Rpc, Value)>(1);
        let (observed_tx, mut observed_rx) = mpsc::channel(1);
        let observer = AbortTask(tokio::spawn(async move {
            while let Some((epoch, generation, observer_rpc, thread)) = observe_rx.recv().await {
                let result = observer_rpc
                    .request_observation(
                        json!({"threadId":thread["threadId"],"includeTurns":true}),
                        thread["targetTurnId"].as_str().map(str::to_owned),
                    )
                    .await;
                if observed_tx.send((epoch, generation, result)).await.is_err() {
                    break;
                }
            }
        }));
        let mut observation_pending = false;
        let mut provider_epoch = 0u64;
        let (reconnected_tx, mut reconnected_rx) = mpsc::channel(1);
        let mut reconnect_worker: Option<AbortTask> = None;
        let mut recovery_cursor = 0usize;
        let mut provider_stopped = false;
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        loop {
            tokio::select! {
                request = requests.recv() => match request {
                    Some(request) => { let result = rt.command(&request.method, request.body).await; let _ = request.reply.send(result); }
                    None => break,
                },
                event = rt.events.recv(), if !rt.history_failed => match event {
                    Some(event) => {
                        if event["method"] == "spike/provider-exit" { let _ = rt.stop_for_transport_error(); continue; }
                        if rt.event(event).await.is_err() { eprintln!("Rust spike: provider event failed"); }
                    }
                    None => { let _ = rt.stop_for_transport_error(); },
                },
                _ = tick.tick() => {
                    if rt.claude { let _ = rt.check_claude_goal(); }
                    if !rt.history_failed && rt.goals.edits.iter().any(|e| !e.sent && e.error.is_null()) && rt.flush_goals().await.is_err() {
                        eprintln!("Rust spike: goal edit retained for reconciliation");
                    }
                    if rt.drain().await.is_err() { eprintln!("Rust spike: queue dispatch failed; retained"); }
                    if !rt.claude && !rt.history_failed && !observation_pending {
                        let targets = recovery::targets(&rt.records);
                        let target = targets.get(recovery_cursor % targets.len().max(1)).cloned();
                        recovery_cursor = recovery_cursor.wrapping_add(1);
                        if target.is_some() || rt.dispatch_blocked || rt.thread["activeTurnId"].is_string() {
                            observation_pending = observe_tx.try_send((provider_epoch, rt.recovery_generation, rt.rpc.clone(), json!({"threadId":rt.thread["id"],"targetTurnId":target.or_else(||rt.thread["activeTurnId"].as_str().map(str::to_owned))}))).is_ok();
                        }
                    }
                    if rt.can_reconnect() && !rt.recovering && rt.reconnect_attempts < 3 && rt.reconnect_due <= std::time::Instant::now() && rt.provider_exit == "confirmed" {
                        rt.reconnect_attempts += 1;
                        rt.recovering = true;
                        rt.publish();
                        let config = config.clone();
                        let session = rt.thread["id"].as_str().unwrap().to_owned();
                        let targets = recovery::targets(&rt.records);
                        let tx = reconnected_tx.clone();
                        reconnect_worker = Some(AbortTask(tokio::spawn(async move {
                            let result = recovery::connect(config, session, targets).await;
                            let _ = tx.send(result).await;
                        })));
                    }
                    if !rt.history_failed && !notification_pending {
                        if let Some(job) = rt.next_notification() {
                            notification_pending = notify_tx.try_send(job).is_ok();
                        }
                    }
                },
                Some((id, success)) = result_rx.recv() => {
                    notification_pending = false;
                    if success && !rt.history_failed {
                        if let Some(record) = rt.records.iter_mut().find(|r| r.submission["id"] == id) {
                            record.notified = true;
                            if rt.persist().is_err() {
                                eprintln!("Rust spike: notification checkpoint failed");
                            }
                        }
                    }
                }
                Some((epoch, generation, observed)) = observed_rx.recv() => {
                    observation_pending = false;
                    if let (true, Ok(observed)) = (epoch == provider_epoch && generation == rt.recovery_generation, observed) {
                        if rt.observe(&observed).await.is_err() && !rt.state_failed {
                            (rt.records, rt.goals) = rt.checkpoint.clone();
                            let _ = rt.stop_for_history_error("Recovery observation refused; queued work preserved".into());
                        }
                    }
                }
                Some(result) = reconnected_rx.recv() => {
                    reconnect_worker.take();
                    rt.recovering = false;
                    rt.reconnect_due = std::time::Instant::now() + Duration::from_secs(2u64.pow(rt.reconnect_attempts));
                    if let Err(error) = &result {
                        let reason = match error.to_string().as_str() {
                            "recovery_cleanup_unverified" => Some("Recovery child exit unverified; reconnect refused; queued work preserved"),
                            "recovery_validation_refused" => Some("Recovery authority/validation refused; queued work preserved"),
                            _ => None,
                        };
                        if let Some(reason) = reason {
                            rt.transport_failed = false;
                            rt.thread["lastError"] = json!(reason);
                        }
                    }
                    if let Ok(connection) = result {
                        let recovery::Connection { rpc, child: mut replacement, events, snapshot, targets, buffered } = connection;
                        if rt.state_failed || !rt.transport_failed {
                            let _ = timeout(Duration::from_secs(5), replacement.kill()).await;
                            rt.publish();
                            continue;
                        }
                        rt.rpc = rpc;
                        provider_epoch += 1;
                        child = replacement;
                        rt.events = events;
                        provider_stopped = false;
                        rt.provider_exit = "not_requested";
                        rt.history_failed = false;
                        rt.transport_failed = false;
                        rt.ready = true;
                        let result = async {
                            rt.observe(&snapshot).await?;
                            rt.refresh_recovery_history(&snapshot)?;
                            // Exact target reads follow resume and may already
                            // contain a newer terminal status. Never rebuild the
                            // window from the older snapshot after applying them.
                            for observed in &targets { rt.observe(observed).await?; }
                            for event in buffered {
                                if let Some(covered) = recovery::covering_terminal(&snapshot, &targets, &event) {
                                    rt.merge_covered_lifecycle(&event, covered)?;
                                } else { rt.event(event).await?; }
                            }
                            rt.persist()?;
                            Ok::<_,anyhow::Error>(())
                        }.await;
                        if result.is_err() && !rt.history_failed {
                            (rt.records, rt.goals) = rt.checkpoint.clone();
                            let _ = rt.stop_for_history_error("Recovery validation failed; queued work preserved".into());
                        }
                    }
                    rt.publish();
                }
            }
            if rt.rpc.failed() && !rt.history_failed {
                let _ = rt.stop_for_transport_error();
            }
            if rt.history_failed && !provider_stopped {
                if !rt.can_reconnect() { notification_worker.0.abort(); }
                rt.publish();
                rt.record_provider_stop(timeout(Duration::from_secs(5), child.kill()).await);
                rt.events.close();
                while rt.events.try_recv().is_ok() {}
                provider_stopped = true;
            }
        }
        drop(notification_worker);
        drop(observer);
        drop(reconnect_worker);
        } => {},
        }
        requests.close();
        while let Ok(request) = requests.try_recv() {
            let _ = request.reply.send(Err(anyhow!("runtime_stopping")));
        }
        rt.ready = false;
        rt.dispatch_blocked = true;
        rt.thread["status"] = json!("error");
        if rt.thread["lastError"].as_str().is_none_or(str::is_empty) {
            rt.thread["lastError"] = json!("Launcher stopping; queued and uncertain work preserved");
        }
        rt.publish();
        // Preserve uncertain receipts and queued work; do not invent completion.
        rt.record_provider_stop(timeout(Duration::from_secs(5), child.kill()).await);
        if config.capture_stderr {
            rt.rpc.stderr_saved(rt.store.save_stderr(&child.stderr_tail()).is_ok());
        }
        rt.publish();
        let diagnostic = rt.rpc.diagnostics();
        eprintln!("Rust spike: shutdown provider_exit={}; process_group=termination_requested_unverified; detached_descendants=not_guaranteed; transport={}; before_cleanup={}", rt.provider_exit, diagnostic["transport"].as_str().unwrap_or("unverified"), diagnostic["beforeCleanup"]);
        stopped_tx.send_replace(Some(!rt.state_failed));
    });
    Ok(Handle { commands, state, shutdown, stopped })
}

impl Runtime {
    async fn provider_request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.provider_selected(method,params,None).await
    }
    async fn provider_selected(&mut self, method: &str, params: Value, selection: Option<(usize,usize)>) -> Result<Value> {
        self.provider_query(method, params, selection, None, false).await
    }
    async fn provider_query(&mut self, method: &str, params: Value, selection: Option<(usize,usize)>, target: Option<String>, optional: bool) -> Result<Value> {
        // This outer deadline is polled even during an event's reply write.
        let result = if self.claude && matches!(method,"model/set"|"effort/set"|"effort/probe") {
            // This control has its own response-only deadline after fatal
            // write/flush; keep pumping events without the required-RPC timer.
            self.wait_provider_response(method,params,selection,target,optional).await
        } else {match timeout(Duration::from_secs(60), self.wait_provider_response(method, params, selection, target, optional)).await {
            Ok(result) => result,
            Err(_) => { self.rpc.fail("rpc_timeout"); Err(anyhow!("provider_rpc_timeout")) },
        }};
        if self.rpc.failed() {
            self.stop_for_transport_error()?;
            bail!("provider_transport_closed");
        }
        result
    }
    async fn wait_provider_response(&mut self, method: &str, params: Value, selection: Option<(usize,usize)>, target: Option<String>, optional: bool) -> Result<Value> {
        self.rpc_completed_turns.clear();
        let rpc = self.rpc.clone();
        let claude=self.claude;
        let response = async {
            if method=="model/set" && claude {rpc.request_claude_model(params).await}
            else if claude && matches!(method,"effort/set"|"effort/probe") {rpc.request_claude_effort(method,params).await}
            else if optional { rpc.request_optional(method, params).await }
            else if target.is_some() { rpc.request_target(method, params, target).await }
            else { rpc.request_selected(method, params, selection).await }
        };
        tokio::pin!(response);
        loop {
            tokio::select! {
                biased;
                result = &mut response => return result,
                event = self.events.recv() => {
                    let event = event.ok_or_else(|| anyhow!("Codex disconnected"))?;
                    if event["method"] == "spike/provider-exit" { bail!("Codex disconnected"); }
                    let completed = (event["method"] == "turn/completed"
                        && event["params"]["threadId"] == self.thread["id"])
                        .then(|| event["params"]["turn"]["id"].as_str().map(str::to_owned)).flatten();
                    self.event(event).await?;
                    if self.history_failed { bail!("runtime_blocked"); }
                    // Completion-before-response correlation never outlives
                    // the retained window, even during a rolling long RPC.
                    if let Some(id) = completed { self.rpc_completed_turns.insert(id); }
                    self.rpc_completed_turns.retain(|id|self.turns.iter().any(|t|t["id"]==id.as_str()));
                }
            }
        }
    }
    fn refresh_recovery_history(&mut self, observed: &Value) -> Result<()> {
        let mut fresh = Transcript::default();
        for wire in observed["thread"]["turns"].as_array().ok_or(HistoryLimit)? {
            let mapped = model::turn(wire);
            let projected = self.projected(vec![mapped], false)?;
            let mut turn = projected.first().ok_or(HistoryLimit)?.clone();
            if let Some(old) = self.turns.iter().find(|t|t["id"] == turn["id"]) {
                let mut prior = old.clone();
                if prior["projectionOnly"] == true { prior["status"] = json!("inProgress"); }
                self.retain_projected_items(&prior, &mut turn)?;
            }
            fresh.insert(turn)?;
        }
        self.history_start = observed["treerHistory"]["start"].as_u64().ok_or(HistoryLimit)? as usize;
        self.history_total = observed["treerHistory"]["total"].as_u64().ok_or(HistoryLimit)? as usize;
        self.turns = fresh;
        Ok(())
    }
    fn merge_covered_lifecycle(&mut self, event: &Value, covered: &Value) -> Result<()> {
        let wire = &event["params"]["turn"];
        let prior = if event["method"] == "turn/completed" { model::final_turn(wire)? } else { model::turn(wire) };
        let prior = self.projected(vec![prior], false)?;
        let prior = prior.first().ok_or(HistoryLimit)?;
        let current = self.turns.iter().find(|turn| turn["id"] == covered["id"]).cloned();
        let mut terminal = current.clone().unwrap_or(model::final_turn(covered)?);
        // Preserve tool/item evidence, and keep the normal identity/output and
        // terminal-contradiction guards. This cannot settle another receipt or
        // clear a newer active turn. Out-of-window turns stay out of the window.
        self.retain_projected_items(prior, &mut terminal)?;
        if current.is_some() { self.upsert_turn(terminal)?; }
        Ok(())
    }
    async fn observe(&mut self, observed: &Value) -> Result<()> {
        if self.history_failed || self.claude {
            return Ok(());
        }
        recovery::validate(observed, &self.thread["id"])?;
        self.recovery_reads += 1;
        self.idle_proof = (observed["thread"]["status"]["type"] == "idle").then(std::time::Instant::now);
        for wire in observed["thread"]["turns"].as_array().unwrap() {
            if self.thread["activeTurnId"] != wire["id"] && !self.records.iter().any(|r| recovery::needs_recovery(r) && r.submission["turn_id"].is_string() && r.submission["turn_id"] == wire["id"]) { continue; }
            // Thread idle/ordering may normalize stale inProgress history for
            // display, but cannot interrupt a live input or release its FIFO.
            if wire["treerStatusProjected"] == true { continue; }
            let terminal = matches!(wire["status"].as_str(), Some("completed" | "failed" | "interrupted"));
            let mapped = if terminal {model::final_turn(wire)?} else {model::turn(wire)};
            let projected = self.projected(vec![mapped], false)?;
            let mut turn = projected.first().ok_or(HistoryLimit)?.clone();
            if let Some(old) = self.turns.iter().find(|t|t["id"] == wire["id"]) {
                if terminal {
                    let mut prior = old.clone();
                    if prior["projectionOnly"] == true { prior["status"] = json!("inProgress"); }
                    self.retain_projected_items(&prior, &mut turn)?;
                }
            }
            // Do not insert an exact older target into the recent window or
            // infer identity from projected/local history. Only this fresh read
            // settles matching acknowledged receipts.
            for record in &mut self.records {
                if record.submission["turn_id"] == wire["id"] { recovery::reconcile_record(record, Some(&turn)); }
            }
            if terminal && self.thread["activeTurnId"] == wire["id"] {
                self.thread["activeTurnId"] = Value::Null;
            }
            if wire["status"] == "inProgress" {
                if self.thread["activeTurnId"].is_string() && self.thread["activeTurnId"] != wire["id"] { bail!("recovery_active_identity_mismatch"); }
                self.thread["activeTurnId"] = wire["id"].clone();
            }
            if self.turns.iter().any(|t|t["id"] == wire["id"]) { self.upsert_turn(turn)?; }
        }
        self.dispatch_blocked = self.records.iter().any(|r| recovery::needs_recovery(r) && (r.submission["state"] != "running" || !r.submission["turn_id"].is_string()));
        if observed["thread"]["status"]["type"] == "active" && !self.thread["activeTurnId"].is_string() { self.dispatch_blocked = true; }
        self.thread["status"] = json!(if self.thread["activeTurnId"].is_string() {"running"} else {"idle"});
        if !self.dispatch_blocked { self.thread["lastError"] = Value::Null; }
        self.persist()?;
        self.publish();
        Ok(())
    }
    fn can_reconnect(&self) -> bool {
        !self.claude && self.history_failed && self.transport_failed && !self.state_failed && self.thread["id"].is_string()
            && matches!(self.rpc.diagnostics()["transport"].as_str(), Some("stdout_eof" | "rpc_timeout" | "stdin_write_error" | "stdin_write_timeout"))
    }
    async fn start(&mut self, config: &Config, saved: Option<Value>) -> Result<()> {
        if let Some(saved) = &saved {
            if self.claude {
                self.terminal_tasks = crate::soul_terminal::validate(saved.get("claudeTerminalTasks").unwrap_or(&json!([])))?;
            }
            self.records = decode_records(saved)?;
            self.dispatch_blocked = saved["dispatchBlocked"].as_bool().unwrap_or(true);
            self.goals = Goals::load(&saved["goals"])?;
            self.checkpoint = (self.records.clone(), self.goals.clone());
        }
        self.provider_request("initialize", json!({"clientInfo":{"name":"treer-rust-spike","title":"Treer Agent","version":"0.0.1"},"capabilities":{"experimentalApi":true}})).await?;
        self.rpc.write(json!({"method":"initialized"})).await?;
        if self.claude {
            // Empty overlay probes the native setter, not model selection.
            // Unsupported/missing ACK hides effort; integrity/write faults stay fatal.
            if let Err(error)=self.provider_request("effort/probe",json!({})).await {
                if self.rpc.failed() || self.history_failed {return Err(error);}
                eprintln!("Claude effort setter unavailable; effort controls hidden");
            }
        }
        self.models = match self.provider_query("model/list", json!({}), None, None, true).await {
            Ok(result) => result["data"].clone(),
            Err(error) if self.rpc.failed() || self.history_failed => return Err(error),
            Err(_) => {
                eprintln!("Rust spike: optional model/list unavailable; continuing startup");
                json!([])
            }
        };
        let session = config
            .session
            .as_deref()
            .or_else(|| saved.as_ref().and_then(|s| s["threadId"].as_str()));
        let result = self.provider_request(if session.is_some() {"thread/resume"} else {"thread/start"}, json!({"threadId":session,"cwd":config.cwd,"approvalPolicy":"never","sandbox":"danger-full-access","experimentalRawEvents":false,"persistExtendedHistory":true})).await?;
        self.history_start=result["treerHistory"]["start"].as_u64().unwrap_or(0) as usize;
        self.history_total=result["treerHistory"]["total"].as_u64().unwrap_or(0) as usize;
        let id = result["thread"]["id"]
            .as_str()
            .ok_or_else(|| anyhow!("missing thread"))?;
        if session.is_some_and(|s| s != id) {
            bail!("session identity mismatch");
        }
        // Resumed history needs explicit status authority; an absent active
        // candidate cannot establish that an existing provider thread is idle.
        if (session.is_some() || result["thread"].get("status").is_some())
            && !matches!(result["thread"]["status"]["type"].as_str(), Some("idle" | "active"))
        {
            bail!("unusable provider thread status");
        }
        // Explicit active status without an identifiable current turn cannot
        // authorize dispatch. Do not guess an ID from historical stale turns.
        if result["thread"]["status"]["type"] == "active"
            && !result["thread"]["turns"].as_array().into_iter().flatten()
                .any(|t| t["status"] == "inProgress")
        {
            bail!("active provider thread has no current turn");
        }
        self.thread = json!({"id":id,"title":result["thread"]["name"].as_str().filter(|name| *name != "Codex (Rust experiment)").unwrap_or(if self.claude {"Claude"} else {"Codex"}),"cwd":config.cwd,"model":result["model"],"reasoningEffort":result["reasoningEffort"],"status":"idle","activeTurnId":null,"lastError":null,"createdAt":now(),"updatedAt":now(),"turns":[],"hasOlderItems":false});
        if self.claude {self.thread["effortSelectionStatus"]=result["effortSelectionStatus"].clone();}
        if self.claude {
            self.models=result["modelOptions"].clone();
            self.model_revision=result["modelRevision"].as_u64().unwrap_or(0);
            self.effort_revision=result["effortRevision"].as_u64().unwrap_or(0);
        }
        for turn in result["thread"]["turns"].as_array().into_iter().flatten() {
            self.turns.insert(model::turn(turn))?;
        }
        // Only freshly resumed provider turns can reconcile acknowledged work.
        // Claude's retained journal history below is a display checkpoint, not
        // independent evidence that an uncertain provider operation finished.
        let mut recovery_blocked = recovery::reconcile(&mut self.records, &self.turns);
        if recovery_blocked && !self.claude {
            // The normal window is deliberately bounded. Select an older
            // acknowledged turn by exact ID using the existing streaming reader,
            // not a complete in-memory history or a journal-synthesized turn.
            let missing = self.records.iter()
                .filter(|record| recovery::needs_recovery(record))
                .filter_map(|record| record.submission["turn_id"].as_str()
                    .filter(|id| !self.turns.iter().any(|turn| turn["id"] == *id))
                    .map(|id| (record.submission["id"].clone(),id.to_owned())))
                .collect::<Vec<_>>();
            if missing.len() > receipts::PENDING_LIMIT { bail!("recovery_target_limit"); }
            for (submission_id,id) in missing {
                let observed=self.provider_query("thread/read",json!({"threadId":self.thread["id"],"includeTurns":true}),None,Some(id.clone()),false).await?;
                if observed["thread"]["id"] != self.thread["id"] { bail!("recovery_thread_mismatch"); }
                if !matches!(observed["thread"]["status"]["type"].as_str(),Some("idle"|"active")) { bail!("recovery_thread_status_unavailable"); }
                let turn=observed["thread"]["turns"].as_array().and_then(|turns|turns.iter().find(|turn|turn["id"]==id)).map(model::turn);
                if let Some(record)=self.records.iter_mut().find(|record|record.submission["id"]==submission_id && record.submission["turn_id"]==id) {
                    recovery::reconcile_record(record,turn.as_ref());
                }
            }
            recovery_blocked=self.records.iter().any(|record| recovery::needs_recovery(record) && record.submission["state"] != "running");
        }
        if self.claude {
            if let Some(saved)=&saved {
                let turns=saved["claudeTurns"].as_array().filter(|turns|turns.len()<=50).ok_or_else(||anyhow!("claude_history_checkpoint_invalid"))?;
                for turn in turns { self.turns.insert(turn.clone())?; }
                self.history_start=saved["claudeHistoryStart"].as_u64().ok_or_else(||anyhow!("claude_history_checkpoint_invalid"))? as usize;
                self.history_total=saved["claudeHistoryTotal"].as_u64().ok_or_else(||anyhow!("claude_history_checkpoint_invalid"))? as usize;
                if self.history_start>self.history_total || turns.len()>self.history_total {bail!("claude_history_checkpoint_invalid");}
            }
        }
        // Project accepted user items once, then admit every subsequent update
        // through the same byte budget. Snapshot reads never replay the journal.
        let dropped=self.turns.dropped;
        let mut projected=self.history()?;
        projected.dropped+=dropped;
        self.turns=projected;
        if let Some(active) = self
            .turns
            .iter()
            .rev()
            .find(|t| t["status"] == "inProgress")
        {
            self.thread["status"] = json!("running");
            self.thread["activeTurnId"] = active["id"].clone();
        }
        // Resume has validated the same provider thread and its history. An
        // acknowledged turn is recoverable by identity, never by resending its
        // input. Legacy launchers also marked these receipts failed on restart.
        self.dispatch_blocked = recovery_blocked;
        if self.dispatch_blocked {
            self.thread["lastError"] = json!("Dispatch outcome uncertain; no matching provider turn; queued work preserved without replay");
        }
        if let Ok(result) = self.provider_request("thread/goal/get", json!({"threadId":self.thread["id"]}))
            .await
        {
            self.goals_supported = if self.claude {result["supported"] == true} else {result.get("goal").is_some()};
            if self.claude {
                // A saved local goal is not native resume authority. Never
                // reapply a command whose outcome may already have effects.
                if self.goals.goal.is_object() {
                    self.goals.goal["syncStatus"]=json!("desynced");
                    self.goals.goal["syncError"]=json!("Native goal state unconfirmed after restart; no command replayed");
                }
                for edit in &mut self.goals.edits {
                    if edit.applying && !edit.applied {edit.error=json!("claude_goal_ack_unknown_after_restart");}
                }
            } else if !result["goal"].is_null() {
                self.goals.goal = goals::map(&result["goal"], &self.goals.goal);
            } else if self.goals.goal["status"] == "active" || self.goals.goal["status"] == "paused"
            {
                self.goals.goal = Value::Null;
            }
        }
        self.ready = true;
        self.persist()?;
        self.publish();
        Ok(())
    }
    fn persist(&mut self) -> Result<()> {
        if self.state_failed {
            (self.records, self.goals) = self.checkpoint.clone();
            bail!("private_state_write_failed");
        }
        receipts::prune(&mut self.records, self.dispatch_blocked, &self.thread["activeTurnId"]);
        let mut state=json!({"schema":1,"threadId":self.thread["id"],"dispatchBlocked":self.dispatch_blocked,"records":self.records,"goals":self.goals});
        if self.claude {
            state["harness"]=json!("claude");state["claudeTurns"]=json!(&*self.turns);
            state["claudeHistoryStart"]=json!(self.history_start+self.turns.dropped);
            state["claudeHistoryTotal"]=json!(self.history_total);
            state["claudeTerminalTasks"]=crate::tasks::finished(&self.terminal_tasks);
        }
        if let Err(error) = self.store.save(&state) {
            eprintln!("Rust spike: checkpoint_failure=1");
            if error.is::<crate::store::PublicationUncertain>() {
                self.checkpoint = (self.records.clone(), self.goals.clone());
            }
            (self.records, self.goals) = self.checkpoint.clone();
            self.stop_for_state_error();
            bail!("private_state_write_failed");
        }
        self.checkpoint = (self.records.clone(), self.goals.clone());
        // Non-authoritative evidence is sampled only after the live checkpoint
        // commits, at most once per five minutes. Its failure cannot undo that
        // commit or fence dispatch. The copy is bounded to one 32 MiB snapshot.
        if self.last_disk_snapshot.is_none_or(|at| at.elapsed() >= Duration::from_secs(300)) {
            self.last_disk_snapshot = Some(std::time::Instant::now());
            self.disk_history_status = if self.store.retain_history().is_ok() { "available" } else { "journal_history_unavailable" };
        }
        Ok(())
    }
    fn stop_for_state_error(&mut self) {
        // Storage is unavailable: do not claim that this fence was persisted.
        // Existing durable intent remains authoritative on restart.
        self.history_failed = true;
        self.state_failed = true;
        self.transport_failed = false;
        self.ready = false;
        self.dispatch_blocked = true;
        self.thread["status"] = json!("error");
        self.thread["lastError"] = json!("private_state_write_failed; queue paused");
    }
    fn publish(&self) {
        let mut thread = self.thread.clone();
        thread["turns"] = json!(&*self.turns);
        let queue: Vec<_> = self.records.iter().filter(|r|r.submission["state"] == "queued").enumerate().map(|(n,r)|json!({"id":r.submission["id"],"text":r.text,"createdAt":r.submission["created_at"],"sender":r.submission["sender"],"position":n+1})).collect();
        let mut snapshot = json!({"ready":self.ready,"runtime":{"state":if self.dispatch_blocked {"blocked"} else if !self.ready {"starting"} else if thread["status"] == "running" {"working"} else if thread["status"] == "error" {"blocked"} else {"idle"},"error":thread["lastError"],"cleanup":{"providerExit":self.provider_exit,"processGroup":if self.provider_exit == "not_requested" {"not_requested"} else {"termination_requested_unverified"},"detachedDescendants":"not_guaranteed"}},"harness":{"id":"codex","name":"Codex","capabilities":{"images":self.attachments.is_some(),"steer":true,"backgroundTasks":false,"goal":if self.goals_supported {json!({"pause":true,"budget":true})} else {Value::Null},"sessionTransfer":null}},"thread":null,"modelOptions":self.models,"backgroundTasks":[],"queue":queue,"goal":self.goals.goal,"goalDelivery":self.goals.delivery(),"history":{"limit":50},"attachmentsAvailable":self.attachments.is_some(),"attachmentStorageStatus":if self.attachments.is_some() {"available"} else {"agent_private_state_unavailable"}});
        snapshot["thread"] = thread;
        if self.claude {
            snapshot["harness"]=json!({"id":"claude","name":"Claude","capabilities":{"images":self.attachments.is_some(),"steer":true,"backgroundTasks":true,"goal":if self.goals_supported {json!({"pause":false,"budget":false})} else {Value::Null},"sessionTransfer":null}});
            if let Some(edit)=self.goals.edits.last() {
                snapshot["goalDelivery"]=json!({"id":edit.id,"status":if !edit.error.is_null() {"failed"} else if edit.applied {"sent"} else {"pending"},"error":edit.error,"promptId":edit.prompt_id});
            }
            snapshot["backgroundTasks"]=self.terminal_tasks.clone();
        }
        let start=(self.history_start+self.turns.dropped).min(self.history_total);
        snapshot["thread"]["hasOlderItems"]=json!(start>0);
        snapshot["history"]=json!({"limit":50,"totalTurns":self.history_total,"cursor":if start>0 {Some(format!("{}:{start:x}",self.history_nonce))}else{None}});
        if self.claude && start>0 {
            snapshot["history"]["cursor"]=Value::Null;
            snapshot["history"]["unavailable"]=json!(true);
            snapshot["thread"]["historyUnavailable"]=json!(true);
        }
        snapshot["runtime"]["provider"] = self.rpc.diagnostics();
        let unresolved: Vec<_> = self.records.iter().filter(|r| recovery::needs_recovery(r))
            .map(|r|json!({"submissionId":r.submission["id"],"turnId":r.submission["turn_id"],"state":if r.submission["turn_id"].is_string(){"exact_turn_pending"}else{"unknown_dispatch"}})).collect();
        snapshot["runtime"]["recovery"] = json!({"state":if self.state_failed {"storage_failure"} else if self.recovering {"reconnecting"} else if self.can_reconnect() && self.reconnect_attempts>=3 {"reconnect_exhausted"} else if self.history_failed && !self.transport_failed {"integrity_failure"} else if self.history_failed {"provider_unavailable"} else if self.dispatch_blocked {"reconciliation_required"} else {"observing"},"readCount":self.recovery_reads,"reconnectAttempts":self.reconnect_attempts,"unresolved":unresolved,"endpoint":"/api/recovery","freshHistorySupported":!self.claude,"automaticReplay":false});
        snapshot["runtime"]["provider"]["finalProjection"] = self.final_projection.clone();
        snapshot["runtime"]["journalHistory"] = json!({"state":self.disk_history_status,"diskLimitBytes":crate::disk_history::DISK_LIMIT,"sampleIntervalSeconds":300});
        self.changed.send_replace(Arc::new(snapshot));
    }
    fn history(&self) -> Result<Transcript> {
        self.projected(self.turns.to_vec(),self.history_start==0 && self.turns.dropped==0)
    }
    fn retain_projected_items(&self, old: &Value, next: &mut Value) -> Result<(usize, usize), model::InvalidFinalSnapshot> {
        let mut prior = old.clone();
        // Legacy steering receipts have no persisted item-position evidence.
        // Their guessed display slot must not veto a later ordered native
        // snapshot. Remove only that receipt's positional constraint; native
        // activity ordering, terminal/output guards and dispatch authority stay
        // with retain_final_items. An omitted echo supplies no new position.
        if let Some(items) = prior["items"].as_array_mut() {
            items.retain(|item| !self.records.iter().any(|record|
                record.receipt["disposition"] == "steered" && record.projection_after.is_none()
                && record.submission["turn_id"] == old["id"]
                && item["kind"] == "userMessage" && item["submissionId"] == record.submission["id"]
                && item["sourceTurnId"] == old["id"] && item["text"] == record.text
                && (item["providerAssignedId"] != true || item["positionProvisional"] == true)
                && next["items"].as_array().is_some_and(|incoming| incoming.iter().any(|native|
                    native["providerAssignedId"] == true && native["kind"] == "userMessage"
                    && native["sourceTurnId"] == old["id"]
                    && native["submissionId"] == record.submission["id"]
                    && native["text"] == record.text))));
        }
        model::retain_final_items(&prior, next)
    }
    fn projected(&self, mut turns: Vec<Value>, synthesize: bool) -> Result<Transcript> {
        // Provider snapshots may omit locally submitted user items. The journal
        // already owns their durable text and attested metadata; project it
        // without changing dispatch state or trusting provider-supplied senders.
        let mut matched = std::collections::HashSet::new();
        for (number,record) in self.records.iter().enumerate() {
            if record.node_fingerprint.is_some() && record.text.is_empty() { continue; }
            let Some(id) = record.submission["turn_id"].as_str() else {
                continue;
            };
            if !turns.iter().any(|t| t["id"] == id) {
                // Receipts remain durable, but evicted completed turns must not
                // be synthesized back into every snapshot from that journal.
                if !synthesize || number<self.records.len().saturating_sub(50) {continue;}
                // A local-only turn belongs before the next receipt-associated
                // provider turn, not at the end of all provider history.
                let next = self.records.iter().skip(number + 1)
                    .filter_map(|r| r.submission["turn_id"].as_str())
                    .find_map(|next| turns.iter().position(|t| t["id"] == next))
                    .unwrap_or(turns.len());
                turns.insert(next,json!({"id":id,"projectionOnly":true,"status":if record.submission["state"] == "running" {"inProgress"} else {"completed"},"startedAt":record.submission["created_at"],"error":null,"items":[]}));
            }
            let Some(submission_id) = record.submission["id"].as_str() else {
                continue;
            };
            let Some(turn) = turns.iter_mut().find(|t| t["id"] == id) else {
                continue;
            };
            let Some(items) = turn["items"].as_array_mut() else {
                continue;
            };
            let item_id = format!("submission-{submission_id}");
            // A local projection can precede the native item/started echo.
            // Replace that projection with one native occurrence, rather than
            // keeping both. Consume at most one native occurrence per receipt;
            // identical accepted steer inputs remain distinct submissions.
            let local = items.iter().position(|item| item["kind"] == "userMessage" && item["submissionId"] == record.submission["id"]);
            let native = items.iter().position(|item| item["kind"] == "userMessage" && item["text"] == record.text && !item["submissionId"].is_string());
            let unpositioned = record.receipt["disposition"] == "steered" && record.projection_after.is_none();
            if let (Some(local), Some(native)) = (local, native) {
                // Keep the receipt-owned first-observed position when a late
                // native echo arrives after tool items. Removing the local
                // slot would otherwise reorder shared completion anchors.
                if unpositioned && (items[local]["providerAssignedId"] != true || items[local]["positionProvisional"] == true) {
                    // A legacy fallback is not evidence that this user spoke
                    // before all the tools. Keep the native occurrence in its
                    // own slot instead of moving it to the guessed prefix.
                    items.remove(local);
                } else {
                    let echoed = items.remove(native);
                    items[local - usize::from(native < local)] = echoed;
                }
            } else if let Some(local) = local.filter(|&at| unpositioned && items[at]["providerAssignedId"] != true) {
                // No historical slot can be reconstructed without an anchor
                // or a native occurrence. Keep the input visible separately at
                // the turn's tail, explicitly marked as unpositioned, rather
                // than inventing an earlier place in the conversation.
                let mut item = items.remove(local);
                item["positionUnavailable"] = json!(true);
                items.push(item);
            }
            let existing = items.iter().position(|item| {
                item["kind"] == "userMessage"
                    && item["text"] == record.text
                    && (!item["submissionId"].is_string() || item["submissionId"] == record.submission["id"])
                    && !matched.contains(&(id.to_string(), item["id"].to_string()))
            });
            if let Some(index) = existing {
                items[index]["id"] = json!(item_id);
                matched.insert((id.to_string(), items[index]["id"].to_string()));
                items[index]["sender"] = record.submission["sender"].clone();
                items[index]["submissionId"] = record.submission["id"].clone();
                items[index]["attachments"] = json!(record.images);
            } else {
                // Queue/start input precedes its response. Steering instead
                // belongs after what was already observed at dispatch, even
                // when the provider omits its user echo from final history.
                // Never move a native occurrence: its ordered position wins.
                let before = record.projection_after.as_ref()
                    .and_then(|after| items.iter().position(|item| item["id"] == *after))
                    .map(|at| at + 1)
                    .or_else(|| {
                        // A final summary can omit the anchor's tool while
                        // retaining items on either side. Use those ordered
                        // shared identities; retain_final_items will keep the
                        // omitted activity. Never sort by text or timestamps.
                        let after = record.projection_after.as_ref()?;
                        let previous = self.turns.iter().find(|t| t["id"] == id)?["items"].as_array()?;
                        let boundary = previous.iter().position(|item| item["id"] == *after)? + 1;
                        previous[boundary..].iter()
                            .find_map(|old| items.iter().position(|item| item["id"] == old["id"]))
                            .or_else(|| previous[..boundary].iter().rev()
                                .find_map(|old| items.iter().position(|item| item["id"] == old["id"]).map(|at| at + 1)))
                    })
                    .unwrap_or_else(|| if unpositioned {items.len()} else {items.iter().take_while(|i| i["kind"] == "userMessage").count()});
                let mut item = json!({"id":item_id,"kind":"userMessage","text":record.text,"sender":record.submission["sender"],"submissionId":record.submission["id"],"detailText":null,"sourceTurnId":id,"attachments":record.images});
                if unpositioned { item["positionUnavailable"] = json!(true); }
                items.insert(before, item);
                matched.insert((id.to_string(), json!(item_id).to_string()));
            }
        }
        let mut bounded=Transcript::default();
        for turn in turns {bounded.insert(turn)?;}
        Ok(bounded)
    }
    async fn command(&mut self, method: &str, body: Value) -> Result<Value> {
        if self.history_failed && !matches!(method, "queue" | "submission" | "recovery") {
            anyhow::bail!("runtime_blocked; queue paused");
        }
        if !matches!(method, "queue" | "submission") && self.store.recheck().is_err() {
            self.stop_for_state_error();
            bail!("private_state_write_failed");
        }
        match method {
            "session/export" => self.session_snapshot(body).await,
            "tasks/clear-finished" if self.claude => {
                let removed = crate::tasks::clear(&mut self.terminal_tasks);
                self.persist()?;
                self.publish();
                Ok(json!({"removed":removed}))
            }
            "recovery" => {
                if self.state_failed { bail!("recovery_storage_unavailable"); }
                match body["action"].as_str() {
                    Some("recheck") => {
                        if self.claude { bail!("claude_fresh_recovery_authority_unavailable"); }
                        if self.history_failed && !self.can_reconnect() { bail!("recovery_validation_failure_requires_source_fix"); }
                        if self.recovering { return Ok(json!({"accepted":true,"state":"reconnecting"})); }
                        self.reconnect_attempts = 0;
                        self.reconnect_due = std::time::Instant::now();
                        Ok(json!({"accepted":true,"automaticReplay":false}))
                    }
                    Some("acknowledge_unknown") => {
                        if let Some(record) = self.records.iter().find(|r|r.submission["id"] == body["submission_id"] && r.submission["recovery_disposition"]["action"] == "acknowledge_unknown") {
                            if body["thread_id"] == self.thread["id"] && body["accept_unknown_outcome"] == true
                                && body.get("expected_turn_id").is_some() && body.get("expected_error").is_some() && body["expected_turn_id"] == record.submission["turn_id"]
                                && body["expected_error"] == record.submission["recovery_disposition"]["originalError"] {
                                return Ok(json!({"accepted":true,"duplicate":true,"automaticReplay":false}));
                            }
                            bail!("recovery_disposition_binding_changed");
                        }
                        let current_idle = if self.claude {self.ready && self.rpc.claude_idle_for_disposition()} else {self.idle_proof.is_some_and(|at|at.elapsed()<=Duration::from_secs(5))};
                        if self.history_failed || self.recovering || !current_idle
                            || body["thread_id"] != self.thread["id"] || body["accept_unknown_outcome"] != true
                        { bail!("recovery_requires_fresh_idle_and_explicit_disposition"); }
                        let record = self.records.iter_mut().find(|r| r.submission["id"] == body["submission_id"] && recovery::needs_recovery(r)).ok_or_else(||anyhow!("recovery_submission_unavailable"))?;
                        if body.get("expected_turn_id").is_none() || body.get("expected_error").is_none() || record.submission["turn_id"] != body["expected_turn_id"] || record.submission["error"] != body["expected_error"] { bail!("recovery_disposition_binding_changed"); }
                        if self.thread["activeTurnId"].is_string() && self.thread["activeTurnId"] != record.submission["turn_id"] { bail!("recovery_other_active_turn"); }
                        record.submission["recovery_disposition"] = json!({"originalState":record.submission["state"],"originalError":record.submission["error"],"action":"acknowledge_unknown","at":now(),"providerOutcome":"unknown","replayed":false,"idleEvidence":if self.claude {"initialized_current_adapter_no_active_request"} else {"fresh_provider_idle_read"}});
                        record.submission["state"] = json!("interrupted");
                        record.submission["error"] = json!("User acknowledged unknown provider outcome; input not replayed");
                        let disposed_turn = record.submission["turn_id"].clone();
                        let disposition = record.submission["recovery_disposition"].clone();
                        let other_pending = self.records.iter().any(|r| recovery::needs_recovery(r) && r.submission["turn_id"] == disposed_turn);
                        self.recovery_generation += 1;
                        if !other_pending && self.thread["activeTurnId"] == disposed_turn {
                            self.thread["activeTurnId"] = Value::Null;
                            self.thread["status"] = json!("idle");
                        }
                        if self.claude && !other_pending {
                            if let Some(mut turn) = self.turns.iter().find(|turn|turn["id"] == disposed_turn).cloned() {
                                turn["status"] = json!("interrupted");
                                turn["projectionOnly"] = json!(true);
                                turn["recoveryDisposition"] = disposition;
                                if let Err(error) = self.turns.insert(turn) {
                                    (self.records, self.goals) = self.checkpoint.clone();
                                    self.stop_for_history_error("Recovery display admission failed; queue preserved".into())?;
                                    return Err(error);
                                }
                            }
                        }
                        self.dispatch_blocked = self.records.iter().any(recovery::needs_recovery);
                        if !self.dispatch_blocked { self.thread["lastError"] = Value::Null; }
                        self.persist()?;
                        self.publish();
                        Ok(json!({"accepted":true,"automaticReplay":false}))
                    }
                    _ => bail!("recovery_action_invalid"),
                }
            },
            "history_page" => {
                if self.claude {bail!("claude_older_history_unavailable");}
                let cursor=body["cursor"].as_str().ok_or_else(||anyhow!("history_cursor_invalid"))?;
                if cursor.len()>48 {bail!("history_cursor_invalid");}
                let suffix=cursor.strip_prefix(&format!("{}:",self.history_nonce)).ok_or_else(||anyhow!("history_cursor_invalid"))?;
                let end=usize::from_str_radix(suffix,16).map_err(|_|anyhow!("history_cursor_invalid"))?;
                if end==0 || end>self.history_total || suffix!=format!("{end:x}") {bail!("history_cursor_invalid");}
                if self.last_history_page.is_some_and(|at|at.elapsed()<Duration::from_secs(2)) {bail!("history_page_busy");}
                self.last_history_page=Some(std::time::Instant::now());
                let result=self.provider_selected("thread/read",json!({"threadId":self.thread["id"],"includeTurns":true}),Some((end.saturating_sub(50),end))).await?;
                if result["thread"]["id"]!=self.thread["id"] {bail!("history_page_unavailable");}
                let turns=result["thread"]["turns"].as_array().ok_or_else(||anyhow!("history_page_unavailable"))?;
                if turns.is_empty() {bail!("history_page_unavailable");}
                let start=result["treerHistory"]["start"].as_u64().ok_or_else(||anyhow!("history_page_unavailable"))? as usize;
                if start>=end || turns.len()>50 || result["treerHistory"]["total"].as_u64().unwrap_or(0)<end as u64 {bail!("history_page_unavailable");}
                let mapped=self.projected(turns.iter().map(model::turn).collect(),false).map_err(|_|anyhow!("history_page_unavailable"))?;
                // Mapping may expand a provider value. Never silently return a
                // shortened page with the original cursor and lose the gap.
                if mapped.dropped!=0 {bail!("history_page_unavailable");}
                let mut snapshot=(**self.changed.borrow()).clone();
                snapshot["thread"]["turns"]=json!(&*mapped);
                snapshot["thread"]["hasOlderItems"]=json!(start>0);
                snapshot["history"]=json!({"limit":50,"page":true,"totalTurns":self.history_total,"cursor":if start>0 {Some(format!("{}:{start:x}",self.history_nonce))}else{None}});
                Ok(snapshot)
            },
            "goal" => self.goal_edit(body).await,
            "goal_reconcile" | "goal_retry" => {
                if self.claude {bail!("claude_goal_native_state_unavailable_no_automatic_retry");}
                if method == "goal_retry"
                    && !self
                        .goals
                        .edits
                        .iter()
                        .any(|e| e.id == body["id"].as_str().unwrap_or(""))
                {
                    bail!("goal_edit_unknown");
                }
                let current = self.provider_request("thread/goal/get", json!({"threadId":self.thread["id"]}))
                    .await?;
                if !current["goal"].is_null() {
                    self.goals.goal = goals::map(&current["goal"], &self.goals.goal);
                } else if [json!("active"), json!("paused")].contains(&self.goals.goal["status"]) {
                    self.goals.goal = Value::Null;
                }
                self.persist()?;
                self.flush_goals().await?;
                self.publish();
                Ok(json!({"accepted":true}))
            }
            "submit" => self.submit(body).await,
            "message_receive" => self.receive_messages(body),
            "queue" => Ok(json!({"submissions":self.submissions()})),
            "message_seen" => Ok(json!(self.records.iter().any(|r| r.source_messages.iter().any(|m| m["messageId"] == body["id"])))),
            "message_pending_acks" => Ok(json!(self.records.iter().flat_map(|r| &r.source_messages).filter(|m| m["ackPending"] == true).map(|m| m["deliveryId"].clone()).collect::<Vec<_>>())),
            "message_ack" => {
                for record in &mut self.records {
                    for message in &mut record.source_messages {
                        if message["deliveryId"] == body["id"] { message["ackPending"] = json!(false); }
                    }
                }
                self.persist()?;
                Ok(json!(true))
            },
            "submission" => Ok(self
                .submissions()
                .iter()
                .find(|s| s["id"] == body["id"])
                .map(|s| json!({"submission":s}))
                .unwrap_or(Value::Null)),
            "cancel" => {
                let Some(r) = self
                    .records
                    .iter_mut()
                    .find(|r| r.submission["id"] == body["id"])
                else {
                    return Ok(Value::Null);
                };
                if r.submission["state"] != "queued" {
                    return Ok(json!({"error":{"code":"submission_not_queued"}}));
                }
                r.submission["state"] = json!("interrupted");
                r.submission["position"] = Value::Null;
                let result = json!({"submission":r.submission});
                self.persist()?;
                self.publish();
                Ok(result)
            }
            "abort" => {
                if let Some(turn) = self.thread["activeTurnId"].as_str() {
                    self.provider_request(
                            "turn/interrupt",
                            json!({"threadId":self.thread["id"],"turnId":turn}),
                        )
                        .await?;
                }
                Ok(json!({"accepted":true}))
            }
            "settings" => {
                if self.claude {
                    if !self.ready || self.history_failed || self.state_failed || self.dispatch_blocked {bail!("claude_settings_unavailable");}
                    let mut validation=body.clone();
                    if validation.get("model").is_none() {validation["model"]=self.thread["model"].clone();}
                    self.provider_request("settings/validate",validation).await?;
                    if body.get("model").is_some() {
                        let result=match self.provider_request("model/set",json!({"model":body["model"]})).await {
                            Ok(result)=>result,
                            Err(error)=>{
                                if error.to_string()=="claude_model_response_timeout" {
                                    // Mutation outcome is unknown, not a claim
                                    // that the provider retained the old model.
                                    self.model_revision=self.rpc.claude_model_revision();
                                    self.effort_revision=self.rpc.claude_effort_revision();
                                    self.thread["model"]=Value::Null;
                                    self.thread["modelSelectionStatus"]=json!("unknown");
                                    self.thread["reasoningEffort"]=Value::Null;
                                    self.thread["effortSelectionStatus"]=json!("unknown");
                                    self.publish();
                                }
                                return Err(error);
                            }
                        };
                        self.thread["model"]=result["model"].clone();
                        self.model_revision=result["modelRevision"].as_u64().unwrap_or(0);
                        self.thread["modelSelectionStatus"]=json!("confirmed");
                        self.models=result["modelOptions"].clone();
                        self.thread["reasoningEffort"]=result["reasoningEffort"].clone();
                        self.thread["effortSelectionStatus"]=result["effortSelectionStatus"].clone();
                        self.effort_revision=result["effortRevision"].as_u64().unwrap_or(self.effort_revision);
                        self.publish();
                    }
                    if body.get("reasoningEffort").is_some() {
                        let result=match self.provider_request("effort/set",json!({"reasoningEffort":body["reasoningEffort"]})).await {
                            Ok(result)=>result,
                            Err(error)=>{
                                if error.to_string()=="claude_model_response_timeout" {
                                    self.effort_revision=self.rpc.claude_effort_revision();
                                    self.thread["reasoningEffort"]=Value::Null;
                                    self.thread["effortSelectionStatus"]=json!("unknown");
                                    self.publish();
                                }
                                return Err(error);
                            }
                        };
                        self.thread["reasoningEffort"]=result["reasoningEffort"].clone();
                        self.thread["effortSelectionStatus"]=result["effortSelectionStatus"].clone();
                        self.effort_revision=result["effortRevision"].as_u64().unwrap_or(self.effort_revision);
                        self.publish();
                    }
                    return Ok(json!({"accepted":true}));
                }
                if body.get("model").is_some() {
                    self.thread["model"] = body["model"].clone();
                }
                if body.get("reasoningEffort").is_some() {
                    self.thread["reasoningEffort"] = body["reasoningEffort"].clone();
                }
                self.publish();
                Ok(json!({"accepted":true}))
            }
            _ => bail!("unsupported operation"),
        }
    }
    async fn goal_edit(&mut self, body: Value) -> Result<Value> {
        if !self.ready || !self.goals_supported {
            bail!("goals_unavailable");
        }
        let input = if body["clear"] == true {
            json!({"status":"cleared"})
        } else {
            goals::validate(&body)?
        };
        if self.claude { return self.claude_goal_edit(body, input).await; }
        if input["roundBudget"].as_u64().is_some() {
            bail!("goal_round_budget_not_yet_supported");
        }
        if [json!("blocked"), json!("stale"), json!("done")].contains(&input["status"])
            && std::env::var("AGENT_UI_GOAL_AGENT_EXIT").as_deref() == Ok("0")
        {
            bail!("goal_exit_disabled");
        }
        if !input["objective"].is_string()
            && self.goals.goal.is_null()
            && input["status"] != "cleared"
        {
            bail!("goal_not_set");
        }
        // The UI's user-authored goal edits share the queue's durable context
        // delivery. No goal changes are interpreted as trusted system input.
        let id = body["operationId"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        self.goals.add(&id, input)?;
        self.persist()?;
        self.flush_goals().await?;
        self.publish();
        Ok(json!({"accepted":true}))
    }
    async fn flush_goals(&mut self) -> Result<()> {
        if !self.goals_supported {
            bail!("goals_unavailable");
        }
        for i in 0..self.goals.edits.len() {
            if self.goals.edits[i].sent {
                continue;
            }
            let result = self.flush_goal(i).await;
            if result.is_err() {
                self.goals.edits[i].error = json!("goal_delivery_requires_reconciliation");
                self.persist()?;
                self.publish();
                return result;
            }
        }
        Ok(())
    }
    async fn flush_goal(&mut self, i: usize) -> Result<()> {
        if self.claude { return self.flush_claude_goal(i).await; }
        let edit = self.goals.edits[i].clone();
        let clear = edit.input["status"] == "cleared"
            || [json!("blocked"), json!("stale"), json!("done")].contains(&edit.input["status"]);
        if !edit.applied {
            let recovered = if edit.applying {
                let current = self.provider_request("thread/goal/get", json!({"threadId":self.thread["id"]}))
                    .await?;
                if clear {
                    current["goal"].is_null()
                } else {
                    edit.input
                        .as_object()
                        .unwrap()
                        .iter()
                        .filter(|(k, _)| {
                            !["roundBudget", "reason", "requiredAction"].contains(&k.as_str())
                        })
                        .all(|(k, v)| {
                            current["goal"][k] == *v
                                || k == "status"
                                    && *v == "active"
                                    && current["goal"]["status"] == "complete"
                        })
                }
            } else {
                false
            };
            if edit.applying && !recovered {
                bail!("goal_mutation_uncertain");
            }
            if !recovered {
                self.goals.edits[i].applying = true;
                self.persist()?;
                let mut params = edit.input.clone();
                for key in ["roundBudget", "reason", "requiredAction"] {
                    params.as_object_mut().unwrap().remove(key);
                }
                if edit.input["objective"].is_string()
                    && edit.input["objective"] != self.goals.goal["objective"]
                {
                    if params.get("status").is_none() {
                        params["status"] = json!("active");
                    }
                    if params.get("tokenBudget").is_none() {
                        params["tokenBudget"] = Value::Null;
                    }
                }
                if clear {
                    params = json!({});
                }
                params["threadId"] = self.thread["id"].clone();
                params["origin"] = json!("user");
                let result = self.provider_request(
                        if clear {
                            "thread/goal/clear"
                        } else {
                            "thread/goal/set"
                        },
                        params,
                    )
                    .await?;
                if !clear {
                    self.goals.goal = goals::map(&result["goal"], &self.goals.goal);
                    if self.goals.goal.is_null() {
                        bail!("goal_reply_invalid");
                    }
                }
            } else if !clear {
                let result = self.provider_request("thread/goal/get", json!({"threadId":self.thread["id"]}))
                    .await?;
                self.goals.goal = goals::map(&result["goal"], &self.goals.goal);
            }
            if clear {
                self.goals.goal = edit.previous.clone();
                if self.goals.goal.is_object() {
                    self.goals.goal["status"] = edit.input["status"].clone();
                    self.goals.goal["reason"] = edit.input["reason"].clone();
                    self.goals.goal["requiredAction"] = edit.input["requiredAction"].clone();
                }
            }
            if edit.input.get("roundBudget").is_some() && self.goals.goal.is_object() {
                self.goals.goal["roundBudget"] = edit.input["roundBudget"].clone();
            }
            self.goals.edits[i].goal = self.goals.goal.clone();
            self.goals.edits[i].applied = true;
            self.persist()?;
        }
        if self.goals.edits[i].mode.is_none() {
            self.goals.edits[i].mode = Some(
                if self.thread["activeTurnId"].is_string() {
                    "steer"
                } else {
                    "queue"
                }
                .into(),
            );
            self.persist()?;
        }
        let edit = self.goals.edits[i].clone();
        let result = self.submit(json!({"operation_id":format!("goal-edit/{}/{}",edit.id,edit.attempt),"text":goals::prompt(&edit.previous,&edit.goal),"delivery":edit.mode,"notify":{"on":[]}})).await?;
        if self
            .records
            .iter()
            .find(|r| r.submission["id"] == result["submission"]["id"])
            .is_some_and(|r| {
                r.submission["state"] == "failed" || r.submission["state"] == "interrupted"
            })
        {
            self.goals.edits[i].attempt = edit
                .attempt
                .checked_add(1)
                .ok_or_else(|| anyhow!("goal_retry_limit"))?;
            self.goals.edits[i].mode = None;
            self.persist()?;
            bail!("goal_prompt_failed");
        }
        self.goals.edits[i].sent = true;
        self.goals.edits[i].prompt_id = result["submission"]["id"].clone();
        self.goals.edits[i].error = Value::Null;
        self.persist()?;
        self.publish();
        Ok(())
    }
    async fn claude_goal_edit(&mut self, body: Value, input: Value) -> Result<Value> {
        if body.get("tokenBudget").is_some() || body.get("roundBudget").is_some()
            || input.get("status").is_some_and(|s| s!="cleared")
            || input.get("reason").is_some() || input.get("requiredAction").is_some() {
            bail!("claude_goal_budget_pause_exit_unsupported");
        }
        if input["status"]!="cleared" && !input["objective"].as_str().is_some_and(|s| s!="clear" && s.len()<=65_000) {
            bail!("claude_goal_objective_invalid");
        }
        let id=body["operationId"].as_str().map(str::to_owned).unwrap_or_else(||uuid::Uuid::new_v4().to_string());
        if let Some(edit)=self.goals.edits.iter().find(|e|e.id==id) {
            if edit.input!=input {bail!("goal_edit_id conflict");}
            return Ok(json!({"accepted":true,"duplicate":true}));
        }
        if self.dispatch_blocked || self.thread["activeTurnId"].is_string() || self.records.iter().any(|r|r.submission["state"]=="queued" || r.submission["state"]=="running") {
            bail!("claude_goal_requires_idle_no_interrupt");
        }
        if self.goals.edits.iter().any(|e|!e.applied) {bail!("claude_goal_ack_unconfirmed_no_retry");}
        self.goals.add(&id,input)?;
        if let Some(objective)=self.goals.edits.last().and_then(|edit|edit.input["objective"].as_str()) {
            self.goals.goal=goals::map(&json!({"objective":objective,"status":"active"}),&self.goals.goal);
        }
        if self.goals.goal.is_object() {
            self.goals.goal["syncStatus"]=json!("desynced");
            self.goals.goal["nativeObjective"]=Value::Null;
            self.goals.goal["syncError"]=json!("Waiting for native goal acknowledgement");
        }
        self.persist()?;
        self.flush_goals().await?;
        self.publish();
        Ok(json!({"accepted":true,"nativeAcknowledgement":"pending"}))
    }
    async fn flush_claude_goal(&mut self, i: usize) -> Result<()> {
        let edit=self.goals.edits[i].clone();
        if edit.applying {return Ok(());}
        let args=if edit.input["status"]=="cleared" {"clear"} else {edit.input["objective"].as_str().ok_or_else(||anyhow!("claude_goal_objective_invalid"))?};
        // The normal input journal is the sole dispatch/turn authority. Fixed
        // operation identity also covers a crash between enqueue and edit save.
        self.goals.edits[i].applying=true;
        self.goals.edits[i].mode=Some("queue".into());
        self.persist()?;
        let result=self.submit(json!({"operation_id":format!("claude-goal/{}",edit.id),"text":format!("/goal {args}"),"delivery":"queue","notify":{"on":[]}})).await?;
        self.goals.edits[i].prompt_id=result["submission"]["id"].clone();
        self.persist()?;
        self.check_claude_goal()?;
        Ok(())
    }
    fn check_claude_goal(&mut self) -> Result<()> {
        let Some(i)=self.goals.edits.iter().position(|e|e.applying && !e.applied && e.error.is_null()) else {self.claude_goal_wait=None;self.claude_goal_ack=None;return Ok(());};
        let id=self.goals.edits[i].id.clone();
        let record=self.records.iter().find(|r|r.operation==format!("claude-goal/{id}"));
        if record.is_some_and(|r|r.submission["state"]=="queued") {return Ok(());}
        let (_,started)=self.claude_goal_wait.get_or_insert_with(||(id,std::time::Instant::now()));
        let expired=started.elapsed()>=Duration::from_secs(30);
        if !expired {self.accept_claude_goal_ack()?;}
        if self.goals.edits[i].applied {return Ok(());}
        let terminal=self.records.iter().find(|r|r.operation==format!("claude-goal/{}",self.goals.edits[i].id)).is_none_or(|r|r.submission["state"]!="running");
        if terminal || expired {
            self.claude_goal_ack=None;
            self.goals.edits[i].error=json!("claude_goal_ack_unknown_no_retry");
            if self.goals.goal.is_object() {self.goals.goal["syncStatus"]=json!("desynced");self.goals.goal["syncError"]=json!("Native goal command outcome unknown; input not replayed");}
            self.persist()?;self.publish();
        }
        Ok(())
    }
    fn accept_claude_goal_ack(&mut self) -> Result<()> {
        let Some(p)=self.claude_goal_ack.clone() else {return Ok(());};
        if self.history_failed || self.state_failed || self.rpc.failed() {self.claude_goal_ack=None;return Ok(());}
        let Some(i)=self.goals.edits.iter().position(|edit| {
            !edit.applied && edit.error.is_null() && self.records.iter().any(|record|
                record.operation==format!("claude-goal/{}",edit.id)
                && record.text==format!("/goal {}",p["args"].as_str().unwrap_or(""))
                && record.submission["turn_id"].is_string() && record.submission["turn_id"]==p["turnId"]
                && record.submission["error"].is_null()
                && matches!(record.submission["state"].as_str(),Some("running"|"done")))
        }) else {return Ok(());};
        // A correlated local turn ID is published only AFTER successful input
        // write/flush. An ACK pumped during that await is merely a candidate.
        if p["args"]=="clear" {self.goals.goal=Value::Null;}
        else {
            self.goals.goal=goals::map(&p["goal"],&self.goals.goal);
            self.goals.goal["syncStatus"]=json!("synced");
            self.goals.goal["nativeObjective"]=p["args"].clone();
            if p["completion"]["condition"]==p["args"] && p["completion"]["status"].is_string() {
                self.goals.goal["status"]=p["completion"]["status"].clone();
            }
        }
        let edit=&mut self.goals.edits[i];
        edit.applied=true;edit.sent=true;edit.goal=self.goals.goal.clone();
        self.claude_goal_wait=None;self.claude_goal_ack=None;
        self.persist()?;
        Ok(())
    }
    fn submissions(&self) -> Vec<Value> {
        let mut position = 0;
        self.records
            .iter()
            .map(|r| {
                let mut submission = r.submission.clone();
                submission["position"] = if submission["state"] == "queued" {
                    position += 1;
                    json!(position)
                } else {
                    Value::Null
                };
                submission
            })
            .collect()
    }
    async fn submit(&mut self, body: Value) -> Result<Value> {
        if !self.ready {
            bail!("runtime unavailable")
        }
        let supplied_text = body["text"].as_str().unwrap_or("");
        let text = supplied_text.trim();
        let ids = body.get("attachment_ids").cloned().unwrap_or(json!([]));
        let has_images = ids.as_array().is_some_and(|ids| !ids.is_empty());
        if text.is_empty() && !has_images {
            bail!("text or images required");
        }
        let images = if has_images {
            let store = self
                .attachments
                .as_ref()
                .ok_or_else(|| anyhow!("attachments_unavailable"))?;
            store
                .lock()
                .await
                .resolve(&ids, self.thread["id"].as_str().unwrap_or(""))?
                .into_iter()
                .map(|(metadata, _)| metadata)
                .collect::<Vec<_>>()
        } else if !ids.is_array() {
            bail!("attachment_invalid_ids");
        } else {
            vec![]
        };
        let operation = body["operation_id"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 1024)
            .ok_or_else(|| anyhow!("operation_id required"))?;
        let delivery = body["delivery"].as_str().unwrap_or("queue");
        if delivery != "queue" && delivery != "steer" {
            bail!("invalid delivery");
        }
        validate_metadata(&body)?;
        let submission_fingerprint =
            fingerprint(text, delivery, &body["sender"], &body["notify"], &images).to_string();
        if let Some(prior) = self.records.iter().find(|r| r.operation == operation) {
            let matches = match &prior.node_fingerprint {
                Some(hash) => images.is_empty() && *hash == node_fingerprint(text, delivery, &body["sender"], &body["notify"]),
                None => prior.message_batch.as_ref().map_or_else(|| prior.fingerprint.clone(), |b| {
                    fingerprint(&b.initial_text(), &prior.delivery, &prior.submission["sender"], &prior.notify, &prior.images).to_string()
                }) == submission_fingerprint,
            };
            if !matches {
                bail!("operation_id conflict");
            }
            return Ok(
                json!({"accepted":true,"operation_id":operation,"duplicate":true,"submission":receipt(prior)}),
            );
        }
        // Old journals/retained operation retries keep their original <=100000
        // byte compatibility; only NEW submissions adopt Node's raw UTF-8 cap.
        if supplied_text.len() > 65_536 { bail!("prompt exceeds 64 KiB"); }
        self.prepare_admission()?;
        if receipts::pressure(self.records.iter()) >= receipts::PENDING_LIMIT
            || self.records.len() >= 4096
        {
            bail!("experiment queue/receipt limit reached");
        }
        let id = uuid::Uuid::new_v4().to_string();
        let busy = self.thread["activeTurnId"].is_string();
        let position = self
            .records
            .iter()
            .filter(|r| r.submission["state"] == "queued")
            .count()
            + 1;
        let disposition = if self.dispatch_blocked {
            "queued"
        } else if busy && delivery == "steer" {
            "steered"
        } else if busy || position > 1 {
            "queued"
        } else {
            "sent"
        };
        let record = Record {
            operation: operation.into(),
            fingerprint: submission_fingerprint,
            text: text.into(),
            delivery: delivery.into(),
            submission: json!({"id":id,"sender":body["sender"],"created_at":now(),"position":null,"text_preview":preview(text,256),"state":"queued","turn_id":null,"assistant_preview":null,"error":null}),
            receipt: json!({"id":id,"disposition":disposition,"position":if disposition == "queued" {json!(position)} else {Value::Null}}),
            notify: body["notify"].clone(),
            notified: false,
            images,
            node_fingerprint: None,
            source_messages: vec![],
            message_batch: None,
            projection_after: None,
        };
        if has_images {
            self.attachments
                .as_ref()
                .unwrap()
                .lock()
                .await
                .retain(&ids, self.thread["id"].as_str().unwrap_or(""))?;
        }
        self.records.push_back(record);
        self.persist()?;
        self.drain().await?;
        self.publish();
        let record = self.records.back().unwrap();
        Ok(json!({"accepted":true,"operation_id":operation,"submission":receipt(record)}))
    }
    async fn drain(&mut self) -> Result<()> {
        if !self.ready || self.dispatch_blocked {
            return Ok(());
        }
        let busy = self.thread["activeTurnId"].is_string();
        if !busy && self.records.iter().any(|r| r.submission["state"] == "running")
        {
            return Ok(());
        }
        let next = self
            .records
            .iter()
            .position(|r| r.submission["state"] == "queued" && (!busy || r.delivery == "steer"));
        let Some(index) = next else { return Ok(()) };
        let mut input = vec![];
        if !self.records[index].text.is_empty() {
            input.push(json!({"type":"text","text":self.records[index].text,"text_elements":[]}));
        }
        if !self.records[index].images.is_empty() {
            let ids = json!(self.records[index]
                .images
                .iter()
                .map(|m| m["id"].clone())
                .collect::<Vec<_>>());
            let store = self
                .attachments
                .as_ref()
                .ok_or_else(|| anyhow!("attachments_unavailable"))?;
            let store=store.lock().await;
            let session=self.thread["id"].as_str().unwrap_or("");
            for (metadata, path) in store.resolve(&ids, session)? {
                if self.claude {
                    use base64::Engine;
                    // The private store revalidates no-follow ownership, MIME,
                    // length and digest before these bytes reach the provider.
                    let (_,bytes)=store.read(metadata["id"].as_str().ok_or_else(||anyhow!("attachment_invalid_ids"))?,session)?;
                    input.push(json!({"type":"image","source":{"type":"base64","media_type":metadata["mime"],"data":base64::engine::general_purpose::STANDARD.encode(bytes)}}));
                } else {
                    input.push(json!({"type":"localImage","path":path}));
                }
            }
        }
        // Capture display order before the cooperative RPC can consume newer
        // provider activity. Persist it with intent, never as provider authority.
        if busy {
            self.records[index].projection_after = self.turns.iter()
                .find(|t| t["id"] == self.thread["activeTurnId"])
                .and_then(|t| t["items"].as_array()).and_then(|items| items.last())
                .and_then(|item| item["id"].as_str()).map(str::to_owned);
        }
        // Persist intent BEFORE contacting the provider. No ambiguous replay.
        self.idle_proof = None;
        self.recovery_generation += 1;
        self.records[index].submission["state"] = json!("running");
        self.dispatch_blocked = true;
        self.persist()?;
        let mut params = json!({"threadId":self.thread["id"],"input":input});
        if busy {
            params["expectedTurnId"] = self.thread["activeTurnId"].clone();
        } else {
            if self.thread["model"].is_string() {
                params["model"] = self.thread["model"].clone();
            }
            if self.thread["reasoningEffort"].is_string() {
                params["effort"] = self.thread["reasoningEffort"].clone();
            }
        }
        let steering_turn = self.thread["activeTurnId"].clone();
        let response = self.provider_request(if busy { "turn/steer" } else { "turn/start" }, params)
            .await;
        if self.history_failed { bail!("provider history blocked dispatch"); }
        match response {
            Ok(response) => {
                let turn = if busy {
                    steering_turn
                } else {
                    response["turn"]["id"].clone()
                };
                if !turn.is_string() {
                    self.mark_uncertain(index);
                    self.persist()?;
                    self.publish();
                    return Ok(());
                }
                if self.thread["activeTurnId"].is_string() && self.thread["activeTurnId"] != turn {
                    self.stop_for_history_error("Provider turn response disagrees with live turn".into())?;
                    bail!("provider turn identity mismatch");
                }
                self.records[index].submission["turn_id"] = turn.clone();
                self.records[index].submission["position"] = Value::Null;
                self.dispatch_blocked = false;
                self.thread["activeTurnId"] = turn.clone();
                self.thread["status"] = json!("running");
                if !busy && !self.turns.iter().any(|t| t["id"] == turn) {
                    if let Err(error) = self.upsert_turn(json!({"id":turn,"status":"inProgress","startedAt":now(),"error":null,"items":[]})) {
                        self.stop_for_history_limit()?;
                        return Err(error);
                    }
                } else if let Some(existing)=self.turns.iter().find(|t|t["id"]==turn).cloned() {
                    if let Err(error)=self.upsert_turn(existing) {
                        self.stop_for_history_limit()?;
                        return Err(error);
                    }
                }
                // The cooperative RPC wait can consume start/delta/completion
                // BEFORE the response identifies this accepted submission.
                // Keep those items and now settle only an explicit terminal
                // event for the returned identity, never an idle inference.
                let terminal = self.turns.iter().find(|t| t["id"] == turn
                    && turn.as_str().is_some_and(|id| self.rpc_completed_turns.contains(id))
                    && matches!(t["status"].as_str(), Some("completed" | "failed" | "interrupted"))).cloned();
                if let Some(terminal) = terminal {
                    // Stored turns are browser models, not provider wire items.
                    // Settle receipts directly without mapping those items twice.
                    let status = match terminal["status"].as_str() {
                        Some("failed") => "failed", Some("interrupted") => "interrupted", _ => "done",
                    };
                    let assistant = terminal["items"].as_array().into_iter().flatten().rev()
                        .find(|item| item["kind"] == "agentMessage")
                        .and_then(|item| item["text"].as_str()).map(|text| preview(text, 2048));
                    self.records[index].submission["state"] = json!(status);
                    self.records[index].submission["error"] = terminal["error"].clone();
                    self.records[index].submission["assistant_preview"] = json!(assistant);
                    self.thread["activeTurnId"] = Value::Null;
                    self.thread["status"] = json!("idle");
                }
            }
            Err(_) => {
                self.mark_uncertain(index);
            }
        }
        if let Err(error) = self.persist() {
            self.publish();
            return Err(error);
        }
        self.publish();
        Ok(())
    }
    fn mark_uncertain(&mut self, index: usize) {
        self.dispatch_blocked = true;
        self.records[index].submission["state"] = json!("failed");
        self.records[index].submission["error"] = json!(
            "Codex dispatch outcome uncertain; queue paused; inspect transcript before retry"
        );
        self.thread["lastError"] = self.records[index].submission["error"].clone();
    }
    fn upsert_turn(&mut self, turn: Value) -> Result<()> {
        let fresh=!self.turns.iter().any(|t|t["id"]==turn["id"]);
        let projected=self.projected(vec![turn],false)?;
        let turn=projected.first().ok_or(HistoryLimit)?.clone();
        self.turns.insert(turn)?;
        if fresh {self.history_total+=1;}
        Ok(())
    }
    fn record_provider_stop(
        &mut self,
        result: std::result::Result<std::io::Result<()>, tokio::time::error::Elapsed>,
    ) {
        self.provider_exit = if matches!(result, Ok(Ok(()))) {
            "confirmed"
        } else {
            "unverified"
        };
        let reason = self.thread["lastError"]
            .as_str()
            .unwrap_or("Provider stopped")
            .split("; provider exit ").next().unwrap_or("Provider stopped");
        self.thread["lastError"] = json!(format!("{reason}; provider exit {}; process-group termination requested but unverified; detached-descendant cleanup not guaranteed", self.provider_exit));
        self.publish();
    }
    fn stop_for_history_limit(&mut self) -> Result<()> {
        self.stop_for_history_error(HistoryLimit.to_string())
    }
    fn stop_for_transport_error(&mut self) -> Result<()> {
        let reason = "Provider transport failed; dispatch outcome uncertain; queue paused";
        // A timed-out wait may have cancelled an event handler. Only the last
        // published receipts/goals may become durable with the failure fence.
        (self.records, self.goals) = self.checkpoint.clone();
        if self.thread["id"].is_string() {
            // Publish the failure class atomically with its fence. A temporary
            // integrity classification must not escape before transport retry.
            self.transport_failed = matches!(self.rpc.diagnostics()["transport"].as_str(), Some("stdout_eof" | "rpc_timeout" | "stdin_write_error" | "stdin_write_timeout"));
            self.history_failed = true;
            self.ready = false;
            self.dispatch_blocked = true;
            self.thread["status"] = json!("error");
            self.thread["lastError"] = json!(reason);
            self.persist()?;
            self.publish();
            return Ok(());
        }
        // Initialization may fail before a thread exists. Preserve the previous
        // durable journal instead of replacing it with an invalid thread ID.
        self.history_failed = true;
        self.ready = false;
        self.dispatch_blocked = true;
        self.thread["status"] = json!("error");
        self.thread["lastError"] = json!(reason);
        self.publish();
        Ok(())
    }
    fn stop_for_history_error(&mut self, reason: String) -> Result<()> {
        self.transport_failed = false;
        self.history_failed = true;
        self.ready = false;
        self.dispatch_blocked = true;
        self.thread["status"] = json!("error");
        self.thread["lastError"] = json!(reason);
        // Keep all receipts, including running intents. No terminal outcome is
        // invented and no pending turn is automatically replayed on restart.
        let saved = self.persist();
        self.publish();
        saved
    }
    async fn event(&mut self, event: Value) -> Result<()> {
        if self.history_failed {
            return Ok(());
        }
        let completing = event["method"] == "turn/completed";
        let result = self.apply_event(event).await;
        if completing && result.is_err() {
            self.final_projection["refusals"] = json!(1);
            self.final_projection["conflicts"] = json!(usize::from(result.as_ref().is_err_and(|e| e.is::<model::InvalidFinalSnapshot>())));
        }
        if self.rpc.failed() {
            self.stop_for_transport_error()?;
        }
        if result
            .as_ref()
            .is_err_and(|error| error.is::<HistoryLimit>())
        {
            self.stop_for_history_limit()?;
        } else if result
            .as_ref()
            .is_err_and(|error| error.is::<InvalidTurnIdentity>())
        {
            self.stop_for_history_error(InvalidTurnIdentity.to_string())?;
        } else if result
            .as_ref()
            .is_err_and(|error| error.is::<model::InvalidFinalSnapshot>())
        {
            self.stop_for_history_error(model::InvalidFinalSnapshot.to_string())?;
        }
        result
    }
    async fn apply_event(&mut self, event: Value) -> Result<()> {
        let method = event["method"].as_str().unwrap_or("");
        let p = &event["params"];
        if event.get("id").is_some() {
            let response = match method {
                "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                    json!({"decision":"accept"})
                }
                "item/permissions/requestApproval" => {
                    json!({"permissions":p["permissions"],"scope":"turn"})
                }
                "execCommandApproval" | "applyPatchApproval" => json!({"decision":"approved"}),
                _ => {
                    eprintln!("Rust spike: unsupported_provider_request");
                    self.rpc.write(json!({"id":event["id"],"error":{"code":-32601,"message":"Unsupported provider request"}})).await?;
                    return Ok(());
                }
            };
            self.rpc
                .write(json!({"id":event["id"],"result":response}))
                .await?;
            return Ok(());
        }
        if p["threadId"].is_string() && p["threadId"] != self.thread["id"] {
            return Ok(());
        }
        match method {
            "thread/claude-goal/ack" if self.claude => {
                self.claude_goal_ack=Some(p.clone());
                self.check_claude_goal()?;
            }
            "thread/claude-goal/completion" if self.claude => {
                if self.goals.goal["status"]=="active" && self.goals.goal["syncStatus"]=="synced" && p["condition"]==self.goals.goal["nativeObjective"] {
                    self.goals.goal["status"]=p["status"].clone();
                    self.persist()?;
                } else if let Some(ack)=&mut self.claude_goal_ack {
                    // Preserve wire order if both ACK and its completion arrive
                    // while the actor is still waiting for input write/flush.
                    if p["condition"]==ack["args"] {ack["completion"]=p.clone();}
                }
            }
            "thread/tasks/updated" if self.claude => {
                // Auxiliary metadata cannot complete a turn or poison its transport.
                if crate::tasks::update(&mut self.terminal_tasks, &p["event"]).is_ok() {
                    self.persist()?;
                } else { eprintln!("Rust launcher: claude_task_metadata_refused"); }
            }
            "thread/model/updated" if self.claude => {
                if p["modelRevision"].as_u64().is_some_and(|r|r>self.model_revision) {
                    self.model_revision=p["modelRevision"].as_u64().unwrap();
                    self.thread["model"]=p["model"].clone();
                    self.thread["modelSelectionStatus"]=json!("confirmed");
                    self.models=p["modelOptions"].clone();
                }
                if p["effortRevision"].as_u64().is_some_and(|r|r>self.effort_revision) {
                    self.effort_revision=p["effortRevision"].as_u64().unwrap();
                    self.thread["reasoningEffort"]=p["reasoningEffort"].clone();
                    self.thread["effortSelectionStatus"]=p["effortSelectionStatus"].clone();
                }
            }
            "thread/goal/updated" => {
                self.goals.goal = goals::map(&p["goal"], &self.goals.goal);
                self.persist()?;
            }
            "thread/goal/cleared" => {
                if self.goals.goal.is_object()
                    && [json!("active"), json!("paused")].contains(&self.goals.goal["status"])
                {
                    self.goals.goal["status"] = json!("cleared");
                }
                self.persist()?;
            }
            "turn/started" => {
                self.idle_proof = None;
                self.recovery_generation += 1;
                if self.turns.iter().any(|t| t["id"] == p["turn"]["id"]
                    && t["projectionOnly"] != true
                    && matches!(t["status"].as_str(), Some("completed" | "interrupted" | "failed")))
                {
                    self.stop_for_history_error("Provider reopened a terminal turn".into())?;
                    bail!("terminal turn reopened");
                }
                if self.thread["activeTurnId"].is_string()
                    && self.thread["activeTurnId"] != p["turn"]["id"]
                {
                    self.stop_for_history_error("Provider reported concurrent active turns".into())?;
                    bail!("concurrent active turns");
                }
                self.upsert_turn(model::turn(&p["turn"]))?;
                self.thread["status"] = json!("running");
                self.thread["activeTurnId"] = p["turn"]["id"].clone();
            }
            "turn/completed" => {
                self.idle_proof = None;
                self.recovery_generation += 1;
                self.final_projection = json!({"retainedOnly":0,"sameIdUpdates":0,"conflicts":0,"refusals":0});
                let turn_id = p["turn"]["id"].clone();
                if !turn_id.is_string() { return Err(InvalidTurnIdentity.into()); }
                if self.thread["activeTurnId"].is_string() && self.thread["activeTurnId"] != turn_id {
                    return Err(model::InvalidFinalSnapshot.into());
                }
                let projected = self.projected(vec![model::final_turn(&p["turn"])?], false)?;
                let mut turn = projected.first().ok_or(HistoryLimit)?.clone();
                let counts = if let Some(old) = self.turns.iter().find(|t| t["id"] == turn_id) {
                    let mut prior = old.clone();
                    if prior["projectionOnly"] == true { prior["status"] = json!("inProgress"); }
                    self.retain_projected_items(&prior, &mut turn)?
                } else { (0, 0) };
                let status = if turn["status"] == "failed" {
                    "failed"
                } else if turn["status"] == "interrupted" {
                    "interrupted"
                } else {
                    "done"
                };
                let assistant = turn["items"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .rev()
                    .find(|i| i["kind"] == "agentMessage")
                    .and_then(|i| i["text"].as_str())
                    .map(|s| preview(s, 2048));
                let error = turn["error"].clone();
                self.upsert_turn(turn)?;
                self.final_projection["retainedOnly"] = json!(counts.0);
                self.final_projection["sameIdUpdates"] = json!(counts.1);
                for record in &mut self.records {
                    if record.submission["turn_id"] == turn_id
                        && recovery::needs_recovery(record)
                    {
                        record.submission["state"] = json!(status);
                        record.submission["assistant_preview"] = json!(assistant);
                        record.submission["error"] = error.clone();
                    }
                }
                if self.thread["activeTurnId"] == turn_id {
                    self.thread["activeTurnId"] = Value::Null;
                    self.thread["status"] = json!("idle");
                }
                if self.dispatch_blocked {
                    self.dispatch_blocked = self.records.iter().any(|r| recovery::needs_recovery(r) && (r.submission["state"] != "running" || !r.submission["turn_id"].is_string()));
                    if !self.dispatch_blocked { self.thread["lastError"] = Value::Null; }
                }
                self.persist()?;
            }
            "item/started" | "item/completed" => {
                let turn_id = p["turnId"].as_str().unwrap_or("");
                if let Some(mut turn) = self.turns.iter().find(|t| t["id"] == turn_id).cloned() {
                    let items = turn["items"]
                        .as_array_mut()
                        .ok_or_else(|| anyhow!("invalid turn items"))?;
                    let mut mapped = model::item(&p["item"], turn_id, items.len());
                    // A late user lifecycle event tells us the item identity,
                    // but not its place among earlier items in a resumed turn.
                    // Only the ordered turn snapshot establishes that position.
                    if mapped["kind"] == "userMessage" && self.records.iter().any(|record|
                        record.receipt["disposition"] == "steered" && record.projection_after.is_none()
                        && record.submission["turn_id"] == turn_id && record.text == mapped["text"]
                        && items.iter().any(|old| old["submissionId"] == record.submission["id"]
                            && (old["providerAssignedId"] != true || old["positionProvisional"] == true))) {
                        mapped["positionProvisional"] = json!(true);
                        mapped["positionUnavailable"] = json!(true);
                    }
                    if let Some(old) = items.iter_mut().find(|i| i["id"] == mapped["id"]) {
                        *old = mapped;
                    } else {
                        items.push(mapped);
                    }
                    self.upsert_turn(turn)?;
                    if self.claude {self.persist()?;}
                }
            }
            "turn/plan/updated" => {
                let turn_id = p["turnId"].as_str().unwrap_or("");
                if let Some(mut turn) = self.turns.iter().find(|t| t["id"] == turn_id).cloned() {
                    let items = turn["items"]
                        .as_array_mut()
                        .ok_or_else(|| anyhow!("invalid turn items"))?;
                    let id = format!("plan-{turn_id}");
                    let mapped = model::item(
                        &json!({"id":id,"type":"plan","plan":p["plan"]}),
                        turn_id,
                        items.len(),
                    );
                    if let Some(old) = items.iter_mut().find(|i| i["id"] == id) {
                        *old = mapped;
                    } else {
                        items.push(mapped);
                    }
                    self.upsert_turn(turn)?;
                }
            }
            "item/agentMessage/delta"
            | "item/reasoning/textDelta"
            | "item/reasoning/summaryTextDelta"
            | "item/commandExecution/outputDelta" => {
                if let Some(mut turn) = self.turns.iter().find(|t| t["id"] == p["turnId"]).cloned()
                {
                    if let Some(item) = turn["items"]
                        .as_array_mut()
                        .and_then(|items| items.iter_mut().find(|i| i["id"] == p["itemId"]))
                    {
                        let field = if method.ends_with("outputDelta") {
                            "detailText"
                        } else {
                            "text"
                        };
                        let text = item[field].as_str().unwrap_or("").to_string()
                            + p["delta"].as_str().unwrap_or("");
                        item[field] = json!(text);
                    }
                    self.upsert_turn(turn)?;
                }
            }
            _ => return Ok(()),
        }
        self.thread["updatedAt"] = json!(now());
        self.publish();
        Ok(())
    }
    fn next_notification(&self) -> Option<Notification> {
        for index in 0..self.records.len() {
            let r = &self.records[index];
            let state = r.submission["state"].as_str().unwrap_or("");
            if r.notified || !["done", "failed", "interrupted"].contains(&state) {
                continue;
            }
            if !r.notify["on"]
                .as_array()
                .is_some_and(|a| a.iter().any(|v| v == state))
            {
                continue;
            }
            let recipient = r.notify["to"]
                .as_str()
                .or(r.submission["sender"]["agent_id"].as_str());
            let Some(recipient) = recipient else { continue };
            let mut payload = json!({"type":"treer.prompt.completed","status":state,"submission_id":r.submission["id"],"submission_ids":[r.submission["id"]],"turn_id":r.submission["turn_id"],"assistant_preview":r.submission["assistant_preview"],"error":r.submission["error"]});
            if r.submission["recovery_disposition"].is_object() { payload["recovery_disposition"] = r.submission["recovery_disposition"].clone(); }
            let payload = payload.to_string();
            let args = vec![
                "send".into(),
                "--to".into(),
                recipient.into(),
                "--idempotency-key".into(),
                format!(
                    "rust-spike/submission/{}/terminal",
                    r.submission["id"].as_str().unwrap()
                ),
            ];
            return Some(Notification {
                id: r.submission["id"].as_str()?.into(),
                args,
                payload,
            });
        }
        None
    }
}
fn receipt(record: &Record) -> Value {
    record.receipt.clone()
}
fn decode_records(saved: &Value) -> Result<VecDeque<Record>> {
    fn decode(saved: &Value) -> Result<VecDeque<Record>> {
        if saved["schema"] != 1
            || !saved["dispatchBlocked"].is_boolean()
            || !saved["threadId"]
                .as_str()
                .is_some_and(|s| !s.is_empty() && s.len() <= 1024)
        {
            bail!("invalid journal");
        }
        let records: VecDeque<Record> = serde_json::from_value(saved["records"].clone())?;
        if records.len() > 4096 {
            bail!("invalid journal");
        }
        let mut ids = std::collections::HashSet::new();
        let mut operations = std::collections::HashSet::new();
        for r in &records {
            let s = &r.submission;
            let id = s["id"].as_str().ok_or_else(|| anyhow!("invalid journal"))?;
            if uuid::Uuid::parse_str(if r.node_fingerprint.is_some() { id.strip_prefix("sub_").unwrap_or("") } else { id }).is_err()
                || !ids.insert(id.to_owned())
                || r.operation.is_empty()
                || r.operation.len() > 1024
                || !operations.insert(r.operation.clone())
                || r.text.is_empty() && r.images.is_empty() && (r.node_fingerprint.is_none() || matches!(s["state"].as_str(), Some("queued" | "running")))
                || r.text.len() > 100_000
                || r.projection_after.as_ref().is_some_and(|id| id.is_empty() || id.len() > 1024)
                || !["queue", "steer"].contains(&r.delivery.as_str())
                || !s["state"].as_str().is_some_and(|s| {
                    ["queued", "running", "done", "failed", "interrupted"].contains(&s)
                })
                || !s["created_at"].is_string()
                || !(s["turn_id"].is_null()
                    || s["turn_id"]
                        .as_str()
                        .is_some_and(|s| !s.is_empty() && s.len() <= 1024))
                || r.receipt["id"] != s["id"]
                || r.node_fingerprint.as_ref().is_some_and(|f| f.len() != 64 || !f.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
                || r.node_fingerprint.is_none() && serde_json::from_str::<Value>(&r.fingerprint).ok()
                    != Some(fingerprint(
                        &r.text,
                        &r.delivery,
                        &s["sender"],
                        &r.notify,
                        &r.images,
                    ))
            {
                bail!("invalid journal");
            }
            validate_metadata(&json!({"sender":s["sender"],"notify":r.notify}))?;
            messages::validate_record(r)?;
            if r.source_messages.len() > 256 || r.source_messages.iter().any(|m| !m["ackPending"].is_boolean() || ["messageId", "deliveryId"].into_iter().any(|key| !m[key].as_str().is_some_and(|s| !s.is_empty() && s.len() <= 256 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')))) {
                bail!("invalid journal");
            }
            if r.images.len() > crate::image_format::COUNT_LIMIT
                || r.images.iter().any(|m| {
                    !m["id"].as_str().is_some_and(crate::attachments::valid_id)
                        || !m["size"]
                            .as_u64()
                            .is_some_and(|n| n > 0 && n <= crate::image_format::FILE_LIMIT as u64)
                        || !m["mime"].as_str().is_some_and(|s| {
                            ["image/png", "image/jpeg", "image/webp", "image/gif"].contains(&s)
                        })
                })
            {
                bail!("invalid journal");
            }
        }
        Ok(records)
    }
    // Do not expose parser fragments, prompt text, sender values or file paths.
    decode(saved).map_err(|_| anyhow!("private_state_invalid"))
}

/// Convert the text queue used by Node v1. Unsupported in-flight sidecars or
/// attachments are refused as a whole; source bytes and their backup survive.
pub(crate) fn node_fields(value: &Value, allowed: &[&str]) -> Result<()> {
    if value.as_object().is_none_or(|o| o.keys().any(|key| !allowed.contains(&key.as_str()))) {
        bail!("node_import_unknown_field");
    }
    Ok(())
}
#[cfg(any(unix, test))]
fn node_nullable_string(value: Option<&Value>, limit: usize) -> bool {
    value.is_some_and(|v| v.is_null() || v.as_str().is_some_and(|s| s.len() <= limit))
}
#[cfg(any(unix, test))]
fn validate_node_record(r: &Value) -> Result<()> {
    node_fields(r, &["operationId", "fingerprint", "text", "delivery", "notify", "receipt", "submission", "notification", "turnScope", "priorTurnId", "sourceMessage", "sourceMessages", "attachmentIds", "attachmentSession"])?;
    let s = &r["submission"];
    node_fields(s, &["id", "sender", "created_at", "position", "text_preview", "state", "turn_id", "assistant_preview", "error"])?;
    node_fields(&r["receipt"], &["id", "disposition", "position"])?;
    if !s["sender"].is_null() { node_fields(&s["sender"], &["agent_id", "path", "name"])?; }
    if !r["notify"].is_null() { node_fields(&r["notify"], &["on", "to"])?; }
    if !s["created_at"].as_str().is_some_and(|s| s.len() <= 128 && chrono::DateTime::parse_from_rfc3339(s).is_ok())
        || s["text_preview"].as_str().is_none_or(|s| s.len() > 1024)
        || !node_nullable_string(s.get("assistant_preview"), 4096)
        || !node_nullable_string(s.get("error"), 100_000)
        || !node_nullable_string(s.get("turn_id"), 1024)
        || !s.get("position").is_some_and(Value::is_null)
        || !r["receipt"].get("position").is_some_and(|v| v.is_null() || v.as_u64().is_some_and(|n| n > 0))
        || s.get("sender").is_none() || r.get("notify").is_none()
        || r.get("priorTurnId").is_some_and(|v| !node_nullable_string(Some(v), 1024))
        || r.get("turnScope").is_some_and(|v| !v.as_str().is_some_and(|s| s.len() == 36 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b) || b == b'-')))
        || r.get("attachmentSession").is_some()
    { bail!("node_import_invalid_queue"); }
    if let Some(to) = r["notify"].get("to") {
        if !to.as_str().is_some_and(|s| !s.is_empty() && s.len() <= 1024 && !s.contains(['\r', '\n', '\0'])) { bail!("node_import_invalid_queue"); }
    }
    let notification = r.get("notification").ok_or_else(|| anyhow!("node_import_invalid_queue"))?;
    if !notification.is_null() {
        node_fields(notification, &["attempts", "nextAttempt", "error", "delivered", "batch"])?;
        // Settled notification batches may be omitted from the destination;
        // refuse them here until their full semantic validator is implemented.
        if notification.get("batch").is_some() || notification["delivered"] != true
            || notification["attempts"].as_u64().is_none()
            || notification["nextAttempt"].as_f64().is_none()
            || !node_nullable_string(notification.get("error"), 100_000)
        { bail!("node_import_invalid_queue"); }
    } else if r["notify"]["on"].as_array().is_some_and(|on| on.contains(&s["state"])) {
        bail!("node_import_missing_notification");
    }
    Ok(())
}
#[cfg(any(unix, test))]
pub(crate) fn import_node_queue(thread: &Value, queue: &Value) -> Result<Value> {
    node_fields(thread, &["threadId"])?;
    node_fields(queue, &["version", "records"])?;
    let thread_id = thread["threadId"].as_str().filter(|s| !s.trim().is_empty() && s.len() <= 1024)
        .ok_or_else(|| anyhow!("node_import_invalid_thread"))?;
    if queue["version"] != 1 { bail!("node_import_invalid_queue"); }
    let source = queue["records"].as_array().filter(|r| r.len() <= 4096)
        .ok_or_else(|| anyhow!("node_import_queue_limit"))?;
    let mut records = Vec::new();
    for value in source {
        validate_node_record(value)?;
        let text = value["text"].as_str().filter(|s| s.len() <= 65_536)
            .ok_or_else(|| anyhow!("node_import_invalid_queue"))?;
        let operation = value["operationId"].as_str().ok_or_else(|| anyhow!("node_import_invalid_queue"))?;
        let hash = value["fingerprint"].as_str().ok_or_else(|| anyhow!("node_import_invalid_queue"))?;
        if value.get("attachmentIds").is_some() || !value["notification"].is_null() && value["notification"]["delivered"] != true {
            bail!("node_import_pending_outbox_or_images_unsupported");
        }
        if !["queued", "sent", "steered"].contains(&value["receipt"]["disposition"].as_str().unwrap_or("")) { bail!("node_import_invalid_queue"); }
        let mut messages = Vec::new();
        if !value["sourceMessage"].is_null() { messages.push(value["sourceMessage"].clone()); }
        if let Some(batch) = value.get("sourceMessages") {
            let batch = batch.as_array().filter(|batch| batch.len() <= 255).ok_or_else(|| anyhow!("node_import_invalid_queue"))?;
            messages.extend(batch.iter().cloned());
        }
        if messages.len() > 256 || messages.iter().any(|m| !m["ackPending"].is_boolean() || ["messageId", "deliveryId"].into_iter().any(|k| !m[k].as_str().is_some_and(|s| !s.is_empty() && s.len() <= 256 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')))) {
            bail!("node_import_invalid_queue");
        }
        for message in &messages { node_fields(message, &["messageId", "deliveryId", "ackPending"])?; }
        let submission = value["submission"].clone();
        if matches!(submission["state"].as_str(), Some("queued" | "running")) || messages.iter().any(|m| m["ackPending"] == true) {
            bail!("node_import_requires_idle_empty_queue");
        }
        let record = Record {
            operation: operation.into(), fingerprint: String::new(), node_fingerprint: Some(hash.into()),
            text: text.into(), delivery: value["delivery"].as_str().unwrap_or("").into(),
            submission, receipt: value["receipt"].clone(), notify: value["notify"].clone(),
            notified: value["notification"]["delivered"] == true, images: vec![], source_messages: messages, message_batch: None, projection_after: None,
        };
        records.push(record);
    }
    let state = json!({"schema":1,"threadId":thread_id,"dispatchBlocked":false,"records":records,"goals":null});
    decode_records(&state)?;
    Ok(state)
}

fn node_fingerprint(text: &str, delivery: &str, sender: &Value, notify: &Value) -> String {
    use sha2::{Digest, Sha256};
    // Match Node's insertion-order JSON, including its sender/notify normalization.
    let sender_json = if sender.is_null() { "null".into() } else {
        format!("{{\"agent_id\":{},\"path\":{},\"name\":{}}}", sender["agent_id"], sender["path"], sender["name"])
    };
    let notify_json = if notify.is_null() { "null".into() } else {
        let mut on = notify["on"].as_array().cloned().unwrap_or_default();
        on.sort_by_key(Value::to_string); on.dedup();
        let to = notify.get("to").filter(|v| v.is_string()).or_else(|| sender.get("agent_id"));
        format!("{{\"on\":{}{}}}", json!(on), to.map(|v| format!(",\"to\":{v}")).unwrap_or_default())
    };
    let input = format!("{{\"text\":{},\"delivery\":{},\"sender\":{},\"notify\":{}}}", json!(text), json!(delivery), sender_json, notify_json);
    format!("{:x}", Sha256::digest(input.as_bytes()))
}
fn fingerprint(
    text: &str,
    delivery: &str,
    sender: &Value,
    notify: &Value,
    images: &[Value],
) -> Value {
    if images.is_empty() {
        json!([text, delivery, sender, notify])
    } else {
        json!([text, delivery, sender, notify, images])
    }
}
fn validate_metadata(body: &Value) -> Result<()> {
    if !body["sender"].is_null() {
        for field in ["agent_id", "path", "name"] {
            let s = body["sender"][field]
                .as_str()
                .ok_or_else(|| anyhow!("invalid sender"))?;
            if s.is_empty() || s.len() > 1024 || s.contains(['\r', '\n', '\0']) {
                bail!("invalid sender")
            }
        }
        if !body["sender"]["path"].as_str().unwrap().starts_with('/') {
            bail!("invalid sender")
        }
    }
    if !body["notify"].is_null() {
        let obj = body["notify"]
            .as_object()
            .ok_or_else(|| anyhow!("invalid notify"))?;
        if obj.keys().any(|k| k != "on" && k != "to") {
            bail!("invalid notify")
        }
        let on = body["notify"]["on"]
            .as_array()
            .ok_or_else(|| anyhow!("invalid notify"))?;
        if on
            .iter()
            .any(|s| ![json!("done"), json!("failed"), json!("interrupted")].contains(s))
        {
            bail!("invalid notify")
        }
        let to = body["notify"]["to"]
            .as_str()
            .or(body["sender"]["agent_id"].as_str());
        if !on.is_empty()
            && !to.is_some_and(|s| {
                !s.is_empty() && s.len() <= 1024 && !s.contains(['\r', '\n', '\0'])
            })
        {
            bail!("invalid notification recipient")
        }
    }
    Ok(())
}

pub async fn message_command(args: &[String], input: Option<&str>) -> Result<Value> {
    let mut child = Command::new(crate::treer_binary())
        .arg("message")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn().map_err(|e| if matches!(e.kind(), std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::TimedOut) {message_retry::Failure::Network} else {message_retry::Failure::Other})?;
    if let Some(input) = input {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .await?;
    } else {
        drop(child.stdin.take());
    }
    // Ten Core deliveries may each carry a 1 MiB body. Keep a fixed response
    // ceiling large enough for that one bounded poll, never an unbounded read.
    let mut stdout = child.stdout.take().unwrap().take(16 * 1024 * 1024 + 1);
    let stderr = child.stderr.take().unwrap();
    let mut bytes = vec![];
    let result = timeout(Duration::from_secs(40), async {
        let output = async {
            stdout.read_to_end(&mut bytes).await?;
            if bytes.len() > 16 * 1024 * 1024 {
                return Err(std::io::Error::other("message_output_limit"));
            }
            Ok(())
        };
        let (_, diagnostic) = tokio::try_join!(output, message_retry::diagnostic_prefix(stderr))?;
        let status = child.wait().await?;
        if !status.success() {
            let mut diagnostic = diagnostic;
            diagnostic.push(b'\n');
            diagnostic.extend_from_slice(&bytes[..bytes.len().min(16_384)]);
            return Err(message_retry::classify(&diagnostic).into());
        };
        Ok::<_, anyhow::Error>(())
    })
    .await;
    match result {
        Ok(Ok(())) => {},
        Ok(Err(error)) => {
            let _ = child.kill().await;
            return Err(error.downcast::<message_retry::Failure>().unwrap_or(message_retry::Failure::Other).into());
        },
        Err(_) => {
            let _ = child.kill().await;
            return Err(message_retry::Failure::Network.into());
        }
    }
    if bytes.is_empty() {
        Ok(Value::Null)
    } else {
        serde_json::from_slice(&bytes).map_err(|_| message_retry::Failure::Other.into())
    }
}
pub async fn watch_messages(handle: Handle, agent: String) {
    let mut failures = 0u32;
    loop {
        let received = async {
            let pending = handle.call("message_pending_acks", Value::Null).await?;
            acknowledge_messages(&handle, &pending).await?;
            let result = message_command(
            &[
                "receive".into(),
                "--wait".into(),
                "30000".into(),
                "--limit".into(),
                "10".into(),
            ],
            None,
        )
        .await?;
            // One actor admission for the WHOLE poll, before any idle drain.
            // Only the persisted result grants permission to ack its IDs.
            let ids = handle.call("message_receive",json!({"agent":agent,"deliveries":result["deliveries"]})).await?;
            acknowledge_messages(&handle, &ids).await?;
            Ok::<_, anyhow::Error>(result["deliveries"].as_array().is_some_and(Vec::is_empty))
        }.await;
        match received {
            Ok(empty) => {
                failures = 0;
                if empty { tokio::time::sleep(Duration::from_millis(250)).await; }
            },
            Err(error) => {
                let failure = error.downcast_ref::<message_retry::Failure>().unwrap_or(&message_retry::Failure::Other);
                if *failure == message_retry::Failure::Auth {
                    eprintln!("Rust message watcher stopped: {failure}");
                    break;
                }
                let id = uuid::Uuid::new_v4();
                let jitter = u16::from_le_bytes([id.as_bytes()[0], id.as_bytes()[1]]);
                let delay = message_retry::retry_delay(failures, jitter);
                eprintln!("Rust message watcher retry: {failure}; failures={failures}; delay_ms={}", delay.as_millis());
                tokio::time::sleep(delay).await;
                failures = failures.saturating_add(1);
            }
        }
    }
}
async fn acknowledge_messages(handle: &Handle, ids: &Value) -> Result<()> {
    for id in ids.as_array().ok_or_else(|| anyhow!("invalid pending ack list"))? {
        let id = id.as_str().ok_or_else(|| anyhow!("invalid pending ack id"))?;
        message_command(&["ack".into(), id.into(), "--operation-id".into(), format!("launcher-message-ack/{id}")], None).await?;
        handle.call("message_ack", json!({"id":id})).await?;
    }
    Ok(())
}
#[cfg(test)]
mod journal_tests {
    use super::*;

    #[test]
    fn node_import_preserves_receipts_empty_terminal_text_and_retry_fingerprint() {
        let text = "Node text 中文";
        let hash = node_fingerprint(text, "queue", &Value::Null, &Value::Null);
        let id = format!("sub_{}", uuid::Uuid::new_v4());
        let record = json!({"operationId":"old-op","fingerprint":hash,"text":"","delivery":"queue","notify":null,"notification":null,"receipt":{"id":id,"disposition":"sent","position":null},"submission":{"id":id,"state":"done","sender":null,"created_at":"2026-10-18T00:00:00Z","position":null,"turn_id":"turn-1","text_preview":text,"assistant_preview":"answer","error":null},"sourceMessage":{"messageId":"msg_old","deliveryId":"dlv_old","ackPending":true}});
        assert!(import_node_queue(&json!({"threadId":"thread"}), &json!({"version":1,"records":[record.clone()]})).is_err(), "pending Core ack must refuse import");
        let mut record = record;
        record["sourceMessage"]["ackPending"] = json!(false);
        let source = json!({"version":1,"records":[record.clone()]});
        let state = import_node_queue(&json!({"threadId":"thread"}), &source).unwrap();
        let restored = decode_records(&state).unwrap();
        assert_eq!(restored[0].receipt, record["receipt"]);
        assert_eq!(restored[0].node_fingerprint.as_ref(), Some(&hash));
        assert_eq!(restored[0].source_messages[0], record["sourceMessage"]);
        assert!(restored[0].text.is_empty());
        let mut queued = record.clone(); queued["submission"]["state"] = json!("queued");
        assert!(import_node_queue(&json!({"threadId":"thread"}), &json!({"version":1,"records":[queued]})).is_err());
        for state in ["queued", "running"] {
            let mut busy = record.clone(); busy["text"] = json!(text); busy["submission"]["state"] = json!(state);
            assert!(import_node_queue(&json!({"threadId":"thread"}), &json!({"version":1,"records":[busy]})).is_err(), "busy state must refuse even with valid prompt text");
        }
        let mut pending = record; pending["notification"] = json!({"delivered":false});
        assert!(import_node_queue(&json!({"threadId":"thread"}), &json!({"version":1,"records":[pending]})).is_err());
    }

    #[test]
    fn recovery_rejects_semantic_corruption_without_disclosing_values() {
        let id = uuid::Uuid::new_v4().to_string();
        let record = json!({"operation":"op", "fingerprint":json!(["secret", "queue", null, null]).to_string(),
            "text":"secret", "delivery":"queue", "submission":{"id":id,"state":"running","created_at":now(),"turn_id":null,"sender":null},
            "receipt":{"id":id},"notify":null});
        let valid =
            json!({"schema":1,"threadId":"thread","dispatchBlocked":true,"records":[record]});
        assert_eq!(decode_records(&valid).unwrap().len(), 1);
        for pointer in [
            "/schema",
            "/threadId",
            "/dispatchBlocked",
            "/records/0/submission/id",
            "/records/0/submission/state",
            "/records/0/submission/created_at",
            "/records/0/submission/turn_id",
            "/records/0/fingerprint",
            "/records/0/receipt/id",
            "/records/0/submission/sender",
        ] {
            let mut corrupt = valid.clone();
            *corrupt.pointer_mut(pointer).unwrap() = json!({"secret":"never print this"});
            assert_eq!(
                decode_records(&corrupt).err().unwrap().to_string(),
                "private_state_invalid",
                "{pointer}"
            );
        }
        let mut duplicate = valid.clone();
        duplicate["records"].as_array_mut().unwrap().push(record);
        assert!(decode_records(&duplicate).is_err());
    }
}
