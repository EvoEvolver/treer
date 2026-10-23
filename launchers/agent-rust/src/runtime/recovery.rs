//! Reconcile acknowledged dispatches against validated same-thread history.
//! No input is submitted here, and missing history never proves completion.
use super::*;

pub(super) struct Connection {
    pub rpc: Rpc,
    pub child: crate::rpc::ProviderChild,
    pub events: mpsc::Receiver<Value>,
    pub snapshot: Value,
    pub targets: Vec<Value>,
    pub buffered: Vec<Value>,
}

struct Budget {
    bytes: usize,
    nodes: usize,
}
impl std::io::Write for Budget {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.bytes {
            return Err(std::io::Error::other("recovery_buffer_limit"));
        }
        self.bytes -= bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl Budget {
    fn admit(&mut self, value: &Value) -> Result<()> {
        let nodes = crate::provider_stream::shape_nodes(value);
        if nodes > self.nodes {
            bail!("recovery_buffer_limit");
        }
        self.nodes -= nodes;
        serde_json::to_writer(self, value).map_err(|_| anyhow!("recovery_buffer_limit"))
    }
}

// This task cannot submit input, change a goal or publish a journal. Its owned
// child is killed on cancellation/refusal. Keep reads and event buffering bounded.
pub(super) async fn connect(
    config: Config,
    session: String,
    targets: Vec<String>,
) -> Result<Connection> {
    let (rpc, mut child, mut events) = Rpc::spawn(
        &config.codex,
        &config.codex_args,
        &config.cwd,
        config.capture_stderr,
    )
    .map_err(|_| anyhow!("recovery_validation_refused"))?;
    let mut buffered = Vec::new();
    // Count the entire retained attempt, not merely each individual frame.
    let mut budget = Budget {
        bytes: 32 * 1024 * 1024,
        nodes: 500_000,
    };
    let result = timeout(Duration::from_secs(30), async {
    read(&rpc, &mut events, &mut buffered, &mut budget, "initialize", json!({"clientInfo":{"name":"treer-recovery","version":"0.0.1"},"capabilities":{"experimentalApi":true}}), None).await?;
    rpc.write(json!({"method":"initialized"})).await?;
    let snapshot = read(&rpc, &mut events, &mut buffered, &mut budget, "thread/resume", json!({"threadId":session,"cwd":config.cwd,"approvalPolicy":"never","sandbox":"danger-full-access","persistExtendedHistory":true}), None).await?;
    validate(&snapshot, &json!(session))?;
    let mut found = Vec::new();
    if targets.len() > receipts::PENDING_LIMIT { bail!("recovery_target_limit"); }
    for id in targets {
        let observed = read(&rpc, &mut events, &mut buffered, &mut budget, "thread/read", json!({"threadId":session,"includeTurns":true}), Some(id)).await?;
        validate(&observed, &json!(session))?;
        found.push(observed);
    }
    Ok::<_, anyhow::Error>((snapshot, found))
    }).await.unwrap_or_else(|_|Err(anyhow!("recovery_deadline")));
    match result {
        Ok((snapshot, found)) => Ok(Connection {
            rpc,
            child,
            events,
            snapshot,
            targets: found,
            buffered,
        }),
        Err(error) => {
            // A failed replacement is a new owned process, not the original
            // child's confirmed exit. No next attempt until this exit is proven.
            if !matches!(
                timeout(Duration::from_secs(5), child.kill()).await,
                Ok(Ok(()))
            ) {
                bail!("recovery_cleanup_unverified");
            }
            if matches!(
                error.to_string().as_str(),
                "recovery_authority_unavailable"
                    | "recovery_target_limit"
                    | "recovery_events_unavailable"
                    | "recovery_buffer_limit"
            ) || (rpc.failed()
                && !matches!(
                    rpc.diagnostics()["transport"].as_str(),
                    Some(
                        "stdout_eof" | "rpc_timeout" | "stdin_write_error" | "stdin_write_timeout"
                    )
                ))
            {
                bail!("recovery_validation_refused");
            }
            Err(error)
        }
    }
}

async fn read(
    rpc: &Rpc,
    events: &mut mpsc::Receiver<Value>,
    buffered: &mut Vec<Value>,
    budget: &mut Budget,
    method: &str,
    params: Value,
    target: Option<String>,
) -> Result<Value> {
    timeout(Duration::from_secs(12), async {
        let request = rpc.request_target(method, params, target);
        tokio::pin!(request);
        loop {
            tokio::select! {
                result = &mut request => {
                    let value = result?;
                    budget.admit(&value)?;
                    return Ok(value);
                },
                event = events.recv() => {
                    let event = event.ok_or_else(|| anyhow!("recovery_transport_unavailable"))?;
                    if event["method"] == "spike/provider-exit" || event.get("id").is_some() || buffered.len() >= 64 { bail!("recovery_events_unavailable"); }
                    budget.admit(&event)?;
                    buffered.push(event);
                }
            }
        }
    }).await.map_err(|_|anyhow!("recovery_read_timeout"))?
}

pub(super) fn validate(observed: &Value, thread: &Value) -> Result<()> {
    if observed["thread"]["id"] != *thread
        || !matches!(
            observed["thread"]["status"]["type"].as_str(),
            Some("idle" | "active")
        )
        || !observed["thread"]["turns"].is_array()
    {
        bail!("recovery_authority_unavailable");
    }
    Ok(())
}

pub(super) fn targets(records: &VecDeque<Record>) -> Vec<String> {
    let mut seen = HashSet::new();
    records
        .iter()
        .filter(|r| needs_recovery(r))
        .filter_map(|r| r.submission["turn_id"].as_str())
        .filter(|id| seen.insert((*id).to_owned()))
        .map(str::to_owned)
        .collect()
}

fn uncertain(record: &Record) -> bool {
    record.submission["error"]
        .as_str()
        .is_some_and(|error| error.contains("dispatch outcome uncertain"))
}

pub(super) fn reconcile(records: &mut VecDeque<Record>, turns: &Transcript) -> bool {
    let mut blocked = false;
    for record in records {
        let turn = record.submission["turn_id"]
            .as_str()
            .and_then(|id| turns.iter().find(|turn| turn["id"].as_str() == Some(id)));
        blocked |= reconcile_record(record, turn);
    }
    blocked
}

pub(super) fn needs_recovery(record: &Record) -> bool {
    record.submission["state"] == "running" || uncertain(record)
}

pub(super) fn reconcile_record(record: &mut Record, turn: Option<&Value>) -> bool {
    if !needs_recovery(record) {
        return false;
    }
    let state = turn.and_then(|turn| match turn["status"].as_str() {
        Some("completed") => Some("done"),
        Some("failed") => Some("failed"),
        Some("interrupted") => Some("interrupted"),
        Some("inProgress") => Some("running"),
        _ => None,
    });
    if let (Some(turn), Some(state)) = (turn, state) {
        record.submission["state"] = json!(state);
        record.submission["error"] = if state == "running" {
            Value::Null
        } else {
            turn["error"].clone()
        };
        if state != "running" {
            record.submission["assistant_preview"] = json!(turn["items"]
                .as_array()
                .into_iter()
                .flatten()
                .rev()
                .find(|item| item["kind"] == "agentMessage")
                .and_then(|item| item["text"].as_str())
                .map(|text| preview(text, 2048)));
        }
    } else {
        // Keep every unresolved operation and its original identity. In
        // particular, do not infer a match from text, ordering or idle.
        record.submission["state"] = json!("failed");
        record.submission["error"] = json!("Restart recovery: dispatch outcome uncertain; matching provider turn unavailable; not replayed");
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recovery_budget_bounds_aggregate_bytes_and_shape() {
        let value = json!({"text":"bounded"});
        let bytes = serde_json::to_vec(&value).unwrap().len();
        let nodes = crate::provider_stream::shape_nodes(&value);
        let mut budget = Budget {
            bytes: bytes * 2,
            nodes: nodes * 3,
        };
        budget.admit(&value).unwrap();
        budget.admit(&value).unwrap();
        assert!(budget.admit(&value).is_err());
        let mut budget = Budget {
            bytes: bytes * 3,
            nodes,
        };
        budget.admit(&value).unwrap();
        assert!(budget.admit(&value).is_err());
    }
    fn record(state: &str, id: Value, error: Value) -> Record {
        Record {
            operation: "op".into(),
            fingerprint: "unchanged".into(),
            text: "input".into(),
            delivery: "queue".into(),
            submission: json!({"state":state,"turn_id":id,"error":error}),
            receipt: json!({"id":"receipt"}),
            notify: Value::Null,
            notified: false,
            images: vec![],
            node_fingerprint: None,
            source_messages: vec![json!({"ackPending":true})],
            message_batch: None,
            projection_after: None,
        }
    }
    fn history(status: &str) -> Transcript {
        let mut history = Transcript::default();
        history.insert(json!({"id":"accepted","status":status,"error":null,"items":[{"id":"answer","kind":"agentMessage","text":"answer"}]})).unwrap();
        history
    }
    #[test]
    fn acknowledged_terminal_turns_recover_without_touching_identity_or_ack() {
        for (status, expected) in [
            ("completed", "done"),
            ("failed", "failed"),
            ("interrupted", "interrupted"),
        ] {
            let mut records = VecDeque::from([record("running", json!("accepted"), Value::Null)]);
            assert!(!reconcile(&mut records, &history(status)));
            assert_eq!(records[0].submission["state"], expected);
            assert_eq!(records[0].submission["assistant_preview"], "answer");
            assert_eq!(records[0].receipt, json!({"id":"receipt"}));
            assert_eq!(records[0].source_messages[0]["ackPending"], true);
            assert_eq!(records[0].fingerprint, "unchanged");
        }
    }
    #[test]
    fn legacy_failed_uncertain_receipt_is_reconciled_and_active_turn_remains_running() {
        let mut records = VecDeque::from([record(
            "failed",
            json!("accepted"),
            json!(
                "Experiment restarted; dispatch outcome uncertain; inspect transcript before retry"
            ),
        )]);
        assert!(!reconcile(&mut records, &history("inProgress")));
        assert_eq!(records[0].submission["state"], "running");
        assert!(records[0].submission["error"].is_null());
        assert!(!reconcile(&mut records, &history("completed")));
        assert_eq!(records[0].submission["state"], "done");
    }
    #[test]
    fn missing_and_unknown_turns_stay_fenced_and_queued_work_never_changes() {
        for id in [Value::Null, json!("another-turn")] {
            let mut records = VecDeque::from([
                record("running", id, Value::Null),
                record("queued", Value::Null, Value::Null),
            ]);
            let queued = serde_json::to_value(&records[1]).unwrap();
            assert!(reconcile(&mut records, &history("completed")));
            assert_eq!(records[0].submission["state"], "failed");
            assert_eq!(serde_json::to_value(&records[1]).unwrap(), queued);
            assert!(reconcile(&mut records, &history("completed")));
        }
        let mut records = VecDeque::from([record("running", json!("accepted"), Value::Null)]);
        assert!(reconcile(&mut records, &history("unknown")));
    }
    #[test]
    fn ordinary_terminal_receipts_never_reopen() {
        let mut records = VecDeque::from([record("done", json!("accepted"), Value::Null)]);
        let original = serde_json::to_value(&records).unwrap();
        assert!(!reconcile(&mut records, &history("inProgress")));
        assert_eq!(serde_json::to_value(&records).unwrap(), original);
    }
}
