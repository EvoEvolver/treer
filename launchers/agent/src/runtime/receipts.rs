//! Bounded completed-receipt window; never discard outstanding obligations.
use super::*;
pub(super) const PENDING_LIMIT: usize = 256;
const TERMINAL_WINDOW: usize = 1024;

fn terminal(r: &Record) -> bool {
    matches!(
        r.submission["state"].as_str(),
        Some("done" | "failed" | "interrupted")
    )
}
fn uncertain(r: &Record) -> bool {
    // Existing journals predate a typed uncertainty flag. These fixed runtime
    // errors remain protected even if a later operator reconciles the fence.
    r.submission["error"]
        .as_str()
        .is_some_and(|s| s.contains("dispatch outcome uncertain"))
}
fn pending_notification(r: &Record) -> bool {
    terminal(r)
        && !r.notified
        && r.notify["on"]
            .as_array()
            .is_some_and(|on| on.iter().any(|s| s == &r.submission["state"]))
}
pub(super) fn pending(r: &Record) -> bool {
    matches!(r.submission["state"].as_str(), Some("queued" | "running"))
        || uncertain(r)
        || pending_notification(r)
        || r.source_messages.iter().any(|m| m["ackPending"] == true)
}
pub(super) fn pressure<'a>(records: impl Iterator<Item = &'a Record>) -> usize {
    records.filter(|r| pending(r)).count()
}
fn eligible(r: &Record, active_turn: &Value) -> bool {
    terminal(r)
        && !pending(r)
        && !(active_turn.is_string() && r.submission["turn_id"] == *active_turn)
}
pub(super) fn excess(records: &VecDeque<Record>, blocked: bool, active_turn: &Value) -> usize {
    if blocked {
        return 0;
    }
    records
        .iter()
        .filter(|r| eligible(r, active_turn))
        .count()
        .saturating_sub(TERMINAL_WINDOW)
}
pub(super) fn prune(records: &mut VecDeque<Record>, blocked: bool, active_turn: &Value) {
    let mut remove = excess(records, blocked, active_turn);
    records.retain(|r| {
        if remove > 0 && eligible(r, active_turn) {
            remove -= 1;
            false
        } else {
            true
        }
    });
}
impl Runtime {
    pub(super) fn prepare_admission(&mut self) -> Result<()> {
        // Mostly a no-op: normal persistence already maintains the window.
        // A legacy full journal must durably prune before the capacity test.
        if excess(
            &self.records,
            self.dispatch_blocked,
            &self.thread["activeTurnId"],
        ) > 0
        {
            self.persist()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record(n: usize) -> Record {
        let id = uuid::Uuid::new_v4().to_string();
        Record {
            operation: format!("op-{n}"),
            fingerprint: fingerprint("text", "queue", &Value::Null, &Value::Null, &[]).to_string(),
            text: "text".into(),
            delivery: "queue".into(),
            submission: json!({"id":id,"state":"done","turn_id":format!("turn-{n}"),"sender":null,"created_at":now(),"error":null}),
            receipt: json!({"id":id}),
            notify: Value::Null,
            notified: false,
            images: vec![],
            node_fingerprint: None,
            source_messages: vec![],
            message_batch: None,
            projection_after: None,
            steer_refused: false,
        }
    }
    #[test]
    fn prunes_oldest_safe_terminals_keeps_window_and_pending_identities() {
        let mut rows = (0..4096).map(record).collect::<VecDeque<_>>();
        rows[0].submission["state"] = json!("queued");
        rows[1].submission["state"] = json!("running");
        rows[2].source_messages = vec![json!({"ackPending":true})];
        rows[3].notify = json!({"on":["done"],"to":"agent"});
        rows[4].submission["state"] = json!("failed");
        rows[4].submission["error"] = json!(
            "Experiment restarted; dispatch outcome uncertain; inspect transcript before retry"
        );
        assert_eq!(pressure(rows.iter()), 5);
        prune(&mut rows, false, &json!("turn-5"));
        assert_eq!(rows.len(), 1030);
        for (n, row) in rows.iter().take(6).enumerate() {
            assert_eq!(row.operation, format!("op-{n}"));
        }
        assert_eq!(rows[6].operation, "op-3072");
        assert_eq!(rows.back().unwrap().operation, "op-4095");
        let restored: VecDeque<Record> = serde_json::from_value(json!(rows)).unwrap();
        assert_eq!(pressure(restored.iter()), 5);
    }
    #[test]
    fn fence_protects_whole_journal_and_obligations_release_only_after_settlement() {
        let mut rows = (0..1026).map(record).collect::<VecDeque<_>>();
        rows[0].source_messages = vec![json!({"ackPending":true})];
        rows[1].notify = json!({"on":["done"],"to":"agent"});
        prune(&mut rows, true, &Value::Null);
        assert_eq!(rows.len(), 1026);
        prune(&mut rows, false, &Value::Null);
        assert_eq!(rows.len(), 1026);
        rows[0].source_messages[0]["ackPending"] = json!(false);
        rows[1].notified = true;
        prune(&mut rows, false, &Value::Null);
        assert_eq!(rows.len(), 1024);
        assert_eq!(rows[0].operation, "op-2");
    }
    #[test]
    fn pending_pressure_counts_each_record_once_and_only_matching_notification() {
        let mut r = record(0);
        r.submission["state"] = json!("running");
        r.source_messages = vec![json!({"ackPending":true}), json!({"ackPending":true})];
        assert_eq!(pressure([&r].into_iter()), 1);
        r.submission["state"] = json!("done");
        r.source_messages.clear();
        r.notify = json!({"on":["failed"],"to":"agent"});
        assert!(!pending(&r));
        r.notify["on"] = json!(["done"]);
        assert!(pending(&r));
        r.notified = true;
        assert!(!pending(&r));
    }
}
