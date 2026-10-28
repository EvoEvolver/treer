//! Settle dispatches whose outcome was unknown when the provider stopped,
//! using the resumed session's history. Nothing is replayed that history
//! shows was delivered; nothing waits for a human.
use super::*;

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

/// After a (re)start, settle every record that was in flight:
/// - a turn in history settles it with that turn's status;
/// - Codex input without a turn id is matched to an unclaimed turn by its
///   text; if history has no such turn it was not delivered and is re-queued;
/// - anything else (an unknown turn id, Claude input whose process died)
///   fails with an explanation and the queue continues.
pub(super) fn reconcile(records: &mut VecDeque<Record>, turns: &[Value], claude: bool) {
    let mut claimed: HashSet<String> = records
        .iter()
        .filter_map(|r| r.submission["turn_id"].as_str().map(str::to_owned))
        .collect();
    for record in records.iter_mut() {
        if !needs_recovery(record) {
            continue;
        }
        let mut turn = record.submission["turn_id"]
            .as_str()
            .and_then(|id| turns.iter().find(|turn| turn["id"].as_str() == Some(id)));
        if turn.is_none()
            && record.submission["turn_id"].is_null()
            && !claude
            && !record.text.is_empty()
        {
            turn = turns.iter().find(|turn| {
                turn["id"].as_str().is_some_and(|id| !claimed.contains(id))
                    && turn["items"].as_array().is_some_and(|items| {
                        items.iter().any(|item| {
                            item["kind"] == "userMessage" && item["text"] == record.text.as_str()
                        })
                    })
            });
            if let Some(id) = turn.and_then(|turn| turn["id"].as_str()) {
                claimed.insert(id.to_owned());
                record.submission["turn_id"] = json!(id);
            }
        }
        match turn {
            // Codex turns here all come from the provider; a Claude display
            // projection (task output) is never a receipt's turn.
            Some(turn) if !claude || turn["projectionOnly"] != true => {
                reconcile_record(record, turn)
            }
            _ if record.submission["turn_id"].is_null() && !claude && !uncertain(record) => {
                record.submission["state"] = json!("queued");
                record.submission["error"] = Value::Null;
            }
            _ => {
                record.submission["state"] = json!(if claude { "interrupted" } else { "failed" });
                record.submission["error"] = json!(if claude {
                    "The provider restarted while this prompt was running; it was not replayed"
                } else {
                    "The provider restarted and its history has no record of this turn; it was not replayed"
                });
            }
        }
        // A Claude turn that was still running ended with its process.
        if claude && record.submission["state"] == "running" {
            record.submission["state"] = json!("interrupted");
            record.submission["error"] =
                json!("The provider restarted while this prompt was running; it was not replayed");
        }
    }
}

pub(super) fn needs_recovery(record: &Record) -> bool {
    record.submission["state"] == "running" || uncertain(record)
}

/// Settle a record from a provider turn (including a status Treer inferred
/// from an idle thread). Unknown statuses change nothing.
pub(super) fn reconcile_record(record: &mut Record, turn: &Value) {
    if !needs_recovery(record) {
        return;
    }
    let state = match turn["status"].as_str() {
        Some("completed") => "done",
        Some("failed") => "failed",
        Some("interrupted") => "interrupted",
        Some("inProgress") => "running",
        _ => return,
    };
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
            projection_after: None,
            steer_refused: false,
        }
    }
    fn history(status: &str) -> Vec<Value> {
        vec![
            json!({"id":"accepted","status":status,"error":null,"items":[{"id":"u","kind":"userMessage","text":"input"},{"id":"answer","kind":"agentMessage","text":"answer"}]}),
        ]
    }
    #[test]
    fn acknowledged_terminal_turns_settle_without_touching_identity_or_ack() {
        for (status, expected) in [
            ("completed", "done"),
            ("failed", "failed"),
            ("interrupted", "interrupted"),
            ("inProgress", "running"),
        ] {
            let mut records = VecDeque::from([record("running", json!("accepted"), Value::Null)]);
            reconcile(&mut records, &history(status), false);
            assert_eq!(records[0].submission["state"], expected);
            assert_eq!(records[0].receipt, json!({"id":"receipt"}));
            assert_eq!(records[0].source_messages[0]["ackPending"], true);
            assert_eq!(records[0].fingerprint, "unchanged");
        }
    }
    #[test]
    fn uncertain_start_is_matched_by_text_or_requeued_when_history_lacks_it() {
        let mut records = VecDeque::from([record("running", Value::Null, Value::Null)]);
        reconcile(&mut records, &history("completed"), false);
        assert_eq!(records[0].submission["turn_id"], "accepted");
        assert_eq!(records[0].submission["state"], "done");
        let mut records = VecDeque::from([record("running", Value::Null, Value::Null)]);
        reconcile(&mut records, &[], false);
        assert_eq!(
            records[0].submission["state"], "queued",
            "not in history: not delivered"
        );
        // A turn already owned by another receipt is never claimed twice.
        let mut records = VecDeque::from([
            record("done", json!("accepted"), Value::Null),
            record("running", Value::Null, Value::Null),
        ]);
        reconcile(&mut records, &history("completed"), false);
        assert_eq!(records[1].submission["state"], "queued");
    }
    #[test]
    fn unknown_turn_ids_and_claude_running_input_fail_without_pausing() {
        let mut records = VecDeque::from([
            record("running", json!("another-turn"), Value::Null),
            record("queued", Value::Null, Value::Null),
        ]);
        let queued = serde_json::to_value(&records[1]).unwrap();
        reconcile(&mut records, &history("completed"), false);
        assert_eq!(records[0].submission["state"], "failed");
        assert_eq!(serde_json::to_value(&records[1]).unwrap(), queued);
        for id in [json!("accepted"), Value::Null] {
            let mut records = VecDeque::from([record("running", id, Value::Null)]);
            reconcile(&mut records, &history("inProgress"), true);
            assert_eq!(records[0].submission["state"], "interrupted");
        }
    }
    #[test]
    fn claude_display_projections_are_never_receipt_authority_and_terminal_receipts_never_reopen() {
        let mut turn = history("completed").remove(0);
        turn["projectionOnly"] = json!(true);
        let mut records = VecDeque::from([record("running", json!("accepted"), Value::Null)]);
        reconcile(&mut records, &[turn.clone()], true);
        assert_eq!(records[0].submission["state"], "interrupted");
        // Codex: an interruption Treer inferred from an idle thread settles it.
        turn["status"] = json!("interrupted");
        let mut records = VecDeque::from([record("running", json!("accepted"), Value::Null)]);
        reconcile(&mut records, &[turn], false);
        assert_eq!(records[0].submission["state"], "interrupted");
        let mut records = VecDeque::from([record("done", json!("accepted"), Value::Null)]);
        let original = serde_json::to_value(&records).unwrap();
        reconcile(&mut records, &history("inProgress"), false);
        assert_eq!(serde_json::to_value(&records).unwrap(), original);
    }
}
