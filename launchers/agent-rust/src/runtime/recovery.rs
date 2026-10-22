//! Reconcile acknowledged dispatches against validated same-thread history.
//! No input is submitted here, and missing history never proves completion.
use super::*;

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
