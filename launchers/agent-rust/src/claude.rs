//! Claude stream-json bridge. Session identity is never inferred from private files.
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub struct Adapter {
    session: String,
    active: Option<String>,
    message: String,
    blocks: BTreeMap<u64, (String, Value, String)>,
    items: BTreeMap<String, Value>,
    order: Vec<String>,
    user: String,
    interrupted: bool,
    pending_interrupts: BTreeMap<u64, String>,
    announced: bool,
}
pub enum Request {
    Local(Value),
    Wire(Value),
}
#[cfg(any(unix, test))]
pub fn import_history(
    state: &mut Value,
    seed: &Value,
    agent: &str,
    cwd: &std::path::Path,
    queue_hash: Option<&str>,
) -> Result<()> {
    crate::runtime::node_fields(
        seed,
        &[
            "version",
            "harness",
            "agent",
            "cwd",
            "threadId",
            "queueSha256",
            "snapshot",
        ],
    )?;
    let snapshot = &seed["snapshot"];
    let thread = &snapshot["thread"];
    if seed["version"] != 1
        || seed["harness"] != "claude"
        || seed["agent"] != agent
        || seed["cwd"] != cwd.to_string_lossy().as_ref()
        || seed["threadId"] != state["threadId"]
        || seed.get("queueSha256") != Some(&json!(queue_hash))
        || snapshot["ready"] != true
        || snapshot["harness"]["id"] != "claude"
        || thread["id"] != state["threadId"]
        || thread["status"] != "idle"
        || !thread.get("activeTurnId").is_some_and(Value::is_null)
        || snapshot["runtime"]["state"] != "idle"
        || !snapshot["runtime"].get("error").is_some_and(Value::is_null)
        || snapshot["queue"] != json!([])
        || !snapshot.get("goal").is_some_and(Value::is_null)
        || !snapshot.get("goalDelivery").is_some_and(Value::is_null)
    {
        bail!("claude_supported_snapshot_mismatch");
    }
    let tasks = crate::soul_terminal::validate(&snapshot["backgroundTasks"])?;
    let turns = thread["turns"]
        .as_array()
        .filter(|t| t.len() <= 50)
        .ok_or_else(|| anyhow::anyhow!("claude_snapshot_window"))?;
    let mut seen = std::collections::HashSet::new();
    let mut history = crate::transcript::Transcript::default();
    for turn in turns {
        let id = turn["id"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 1024)
            .ok_or_else(|| anyhow::anyhow!("claude_snapshot_identity"))?;
        if !seen.insert(id)
            || !matches!(
                turn["status"].as_str(),
                Some("completed" | "failed" | "interrupted")
            )
        {
            bail!("claude_snapshot_active_or_duplicate");
        }
        let mut ids = std::collections::HashSet::new();
        for item in turn["items"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("claude_snapshot_items"))?
        {
            let item_id = item["id"]
                .as_str()
                .filter(|s| !s.is_empty() && s.len() <= 1024)
                .ok_or_else(|| anyhow::anyhow!("claude_snapshot_identity"))?;
            if !ids.insert(item_id)
                || !item["kind"].is_string()
                || item
                    .get("sourceTurnId")
                    .is_some_and(|v| !v.is_null() && v != id)
            {
                bail!("claude_snapshot_identity");
            }
        }
        history.insert(turn.clone())?;
    }
    if history.dropped != 0 {
        bail!("claude_snapshot_window");
    }
    // Public snapshots establish a retained window, not native history completeness.
    // Preserve an explicit unavailable older prefix rather than claim a provider
    // pagination API or read private Claude JSONL.
    let total = snapshot["history"]["totalTurns"].as_u64().unwrap_or(
        turns.len() as u64
            + u64::from(thread["hasOlderItems"] == true || thread["historyUnavailable"] == true),
    );
    if total < turns.len() as u64
        || total > 100_000
        || ((thread["hasOlderItems"] == true || thread["historyUnavailable"] == true)
            && total == turns.len() as u64)
    {
        bail!("claude_snapshot_window");
    }
    state["harness"] = json!("claude");
    state["claudeTurns"] = json!(turns);
    state["claudeHistoryStart"] = json!(total - turns.len() as u64);
    state["claudeHistoryTotal"] = json!(total);
    state["claudeTerminalTasks"] = tasks;
    Ok(())
}
impl Adapter {
    // Current process state only. This says nothing about historical effects
    // and must never be used as terminal receipt/history authority.
    pub fn idle_for_disposition(&self) -> bool {
        self.active.is_none() && self.pending_interrupts.is_empty()
    }
    pub fn new(session: String) -> Result<Self> {
        if session.is_empty()
            || session.len() > 1024
            || !session
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
        {
            bail!("claude_session_invalid");
        }
        Ok(Self {
            session,
            active: None,
            message: String::new(),
            blocks: BTreeMap::new(),
            items: BTreeMap::new(),
            order: vec![],
            user: String::new(),
            interrupted: false,
            pending_interrupts: BTreeMap::new(),
            announced: false,
        })
    }
    pub fn request(&mut self, id: u64, method: &str, params: &Value) -> Result<Request> {
        Ok(match method {
            "initialize" => Request::Wire(
                json!({"type":"control_request","request_id":id.to_string(),"request":{"subtype":"initialize"}}),
            ),
            "model/list" => Request::Local(json!({"data":[]})),
            "thread/start" | "thread/resume" => {
                if params["threadId"]
                    .as_str()
                    .is_some_and(|s| s != self.session)
                {
                    bail!("claude_session_mismatch");
                }
                Request::Local(
                    json!({"thread":{"id":self.session,"name":"Claude","status":{"type":"idle"},"turns":[]},"model":null,"reasoningEffort":null}),
                )
            }
            "turn/start" => {
                if self.active.is_some() {
                    bail!("claude_concurrent_turn");
                }
                let inputs = params["input"]
                    .as_array()
                    .ok_or_else(|| anyhow::anyhow!("claude_input"))?;
                if inputs.iter().any(|i| i["type"] != "text") {
                    bail!("claude_image_input_not_implemented");
                }
                let text = inputs
                    .iter()
                    .filter_map(|i| i["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                let turn = uuid::Uuid::new_v4().to_string();
                self.active = Some(turn);
                self.items.clear();
                self.order.clear();
                self.blocks.clear();
                self.interrupted = false;
                self.announced = false;
                self.user = text.clone();
                // Correlated local dispatch result is emitted only after stdin write completes.
                Request::Wire(
                    json!({"type":"user","session_id":self.session,"message":{"role":"user","content":text},"parent_tool_use_id":null}),
                )
            }
            "turn/interrupt" => {
                let turn = self
                    .active
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("claude_no_active_turn"))?;
                self.pending_interrupts.insert(id, turn);
                Request::Wire(
                    json!({"type":"control_request","request_id":id.to_string(),"request":{"subtype":"interrupt"}}),
                )
            }
            _ => bail!("claude_method_unsupported"),
        })
    }
    pub fn dispatched(&self) -> Value {
        json!({"turn":{"id":self.active,"status":"inProgress","items":[{"id":format!("user-{}",self.active.as_deref().unwrap_or("")),"type":"userMessage","text":self.user}]}})
    }
    fn item_event(&self, item: Value) -> Value {
        json!({"method":"item/completed","params":{"threadId":self.session,"turnId":self.active,"item":item}})
    }
    fn map_block(&self, id: &str, b: &Value) -> Value {
        match b["type"].as_str() {
            Some("text") => json!({"id":id,"type":"agentMessage","text":b["text"]}),
            Some("thinking") => json!({"id":id,"type":"reasoning","text":b["thinking"]}),
            Some("tool_use") => match b["name"].as_str() {
                Some("Bash") => {
                    json!({"id":id,"type":"commandExecution","command":b["input"]["command"],"aggregatedOutput":"","status":"inProgress"})
                }
                Some("Edit" | "Write" | "MultiEdit") => {
                    let input = &b["input"];
                    let path = input["file_path"]
                        .as_str()
                        .or(input["path"].as_str())
                        .unwrap_or("");
                    let edits = input["edits"]
                        .as_array()
                        .cloned()
                        .unwrap_or_else(|| vec![input.clone()]);
                    let changes = edits
                        .iter()
                        .map(|edit| {
                            let add = b["name"] == "Write";
                            let before = if add {
                                ""
                            } else {
                                edit["old_string"].as_str().unwrap_or("")
                            };
                            let after = if add {
                                input["content"].as_str().unwrap_or("")
                            } else {
                                edit["new_string"].as_str().unwrap_or("")
                            };
                            let old = before.lines().collect::<Vec<_>>();
                            let new = after.lines().collect::<Vec<_>>();
                            let diff = format!(
                                "--- {}\n+++ b/{path}\n@@ -{},{} +{},{} @@\n{}",
                                if add {
                                    "/dev/null".into()
                                } else {
                                    format!("a/{path}")
                                },
                                usize::from(!old.is_empty()),
                                old.len(),
                                usize::from(!new.is_empty()),
                                new.len(),
                                old.iter()
                                    .map(|l| format!("-{l}"))
                                    .chain(new.iter().map(|l| format!("+{l}")))
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            );
                            json!({"path":path,"kind":if add{"add"}else{"update"},"diff":diff})
                        })
                        .collect::<Vec<_>>();
                    json!({"id":id,"type":"fileChange","status":"inProgress","changes":changes})
                }
                _ => {
                    json!({"id":id,"type":"claudeToolCall","tool":b["name"],"input":b["input"],"status":"inProgress"})
                }
            },
            _ => json!({"id":id,"type":"other"}),
        }
    }
    fn retain(&mut self, id: String, item: Value) -> Result<Value> {
        if id.is_empty() || id.len() > 1024 {
            bail!("claude_item_identity");
        }
        if self.items.len() >= 2048 && !self.items.contains_key(&id) {
            bail!("claude_item_limit");
        }
        if let Some(old) = self.items.get(&id) {
            if matches!(old["status"].as_str(), Some("completed" | "failed")) {
                // Final assistant summary must never reopen/erase a completed tool.
                if item["status"] == "inProgress" {
                    for key in ["type", "command", "changes", "tool", "input"] {
                        if item.get(key).is_some_and(|v| !v.is_null()) && item[key] != old[key] {
                            bail!("claude_terminal_conflict");
                        }
                    }
                    return Ok(old.clone());
                }
                if old != &item {
                    bail!("claude_terminal_conflict");
                }
            }
        }
        let bytes = self
            .items
            .iter()
            .filter(|(key, _)| *key != &id)
            .map(|(_, v)| v.to_string().len())
            .sum::<usize>()
            + item.to_string().len();
        if bytes > 4 * 1024 * 1024 {
            bail!("claude_turn_limit");
        }
        if !self.items.contains_key(&id) {
            self.order.push(id.clone());
        }
        self.items.insert(id, item.clone());
        Ok(item)
    }
    pub fn consume(&mut self, event: Value) -> Result<Vec<Value>> {
        if event["session_id"]
            .as_str()
            .is_some_and(|s| s != self.session)
        {
            bail!("claude_session_mismatch");
        }
        let mut out = vec![];
        if self.active.is_some() && !self.announced {
            self.announced = true;
            out.push(json!({"method":"turn/started","params":{"threadId":self.session,"turn":self.dispatched()["turn"]}}));
        }
        match event["type"].as_str() {
            Some("control_response") => {
                let r = &event["response"];
                let id = r["request_id"]
                    .as_str()
                    .and_then(|s| s.parse::<u64>().ok())
                    .ok_or_else(|| anyhow::anyhow!("claude_control_identity"))?;
                if !matches!(r["subtype"].as_str(), Some("success" | "error")) {
                    bail!("claude_control_response_kind");
                }
                if let Some(turn) = self.pending_interrupts.remove(&id) {
                    if r["subtype"] == "success" && self.active.as_ref() == Some(&turn) {
                        self.interrupted = true;
                    }
                }
                out.push(if r["subtype"] == "error" {
                    json!({"id":id,"error":{"code":"claude_control_refused"}})
                } else {
                    json!({"id":id,"result":r["response"]})
                });
            }
            Some("stream_event") => {
                if self.active.is_none() {
                    bail!("claude_unowned_stream");
                }
                let e = &event["event"];
                let index = e["index"].as_u64().unwrap_or(0);
                if e["type"] == "message_start" {
                    self.message = e["message"]["id"]
                        .as_str()
                        .filter(|s| !s.is_empty() && s.len() <= 512)
                        .ok_or_else(|| anyhow::anyhow!("claude_message_id"))?
                        .into();
                    self.blocks.clear();
                }
                if e["type"] == "content_block_start" {
                    if self.message.is_empty() || !e["index"].is_u64() {
                        bail!("claude_block_identity");
                    }
                    if self.blocks.len() >= 2048 {
                        bail!("claude_block_limit");
                    }
                    let b = e["content_block"].clone();
                    let id = b["id"]
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("{}:{index}", self.message));
                    self.blocks.insert(index, (id, b, String::new()));
                }
                if let Some((id, b, partial)) = self.blocks.get_mut(&index) {
                    let d = &e["delta"];
                    for (kind, field, source) in [
                        ("text_delta", "text", "text"),
                        ("thinking_delta", "thinking", "thinking"),
                    ] {
                        if d["type"] == kind {
                            let text = b[field].as_str().unwrap_or("").to_owned()
                                + d[source].as_str().unwrap_or("");
                            if text.len() > 1024 * 1024 {
                                bail!("claude_block_limit");
                            }
                            b[field] = json!(text);
                        }
                    }
                    if d["type"] == "input_json_delta" {
                        partial.push_str(d["partial_json"].as_str().unwrap_or(""));
                        if partial.len() > 128 * 1024 {
                            bail!("claude_tool_input_limit");
                        }
                        if let Ok(input) = crate::bounded_json::decode(partial.as_bytes()) {
                            b["input"] = input;
                        }
                    }
                    let (id, b) = (id.clone(), b.clone());
                    let item = self.map_block(&id, &b);
                    let item = self.retain(id, item)?;
                    out.push(self.item_event(item));
                }
            }
            Some("assistant" | "user") => {
                let user = event["type"] == "user";
                if let Some(blocks) = event["message"]["content"].as_array() {
                    for (index, b) in blocks.iter().enumerate() {
                        if b["type"] == "tool_result" {
                            let id = b["tool_use_id"]
                                .as_str()
                                .ok_or_else(|| anyhow::anyhow!("claude_tool_id"))?
                                .to_owned();
                            let mut item = self
                                .items
                                .get(&id)
                                .cloned()
                                .ok_or_else(|| anyhow::anyhow!("claude_unknown_tool_result"))?;
                            item["status"] = json!(if b["is_error"] == true {
                                "failed"
                            } else {
                                "completed"
                            });
                            let text = b["content"]
                                .as_str()
                                .map(str::to_owned)
                                .unwrap_or_else(|| b["content"].to_string());
                            if item["type"] == "commandExecution" {
                                item["aggregatedOutput"] = json!(text);
                            } else {
                                item["output"] = json!(text);
                            }
                            let item = self.retain(id, item)?;
                            out.push(self.item_event(item));
                        } else if !user {
                            if self.active.is_none() {
                                bail!("claude_unowned_assistant");
                            }
                            let message = event["message"]["id"]
                                .as_str()
                                .filter(|s| !s.is_empty() && s.len() <= 512)
                                .ok_or_else(|| anyhow::anyhow!("claude_message_id"))?;
                            // A final-only message can repeat another message's
                            // body. Only this same message owns streamed blocks.
                            let streamed = (message == self.message)
                                .then(|| self.blocks.values()
                                    .find(|(_, old, _)| old == b)
                                    .map(|(id, _, _)| id.clone()))
                                .flatten();
                            let id = b["id"]
                                .as_str()
                                .map(str::to_owned)
                                .or(streamed)
                                .unwrap_or_else(|| format!("{message}:{index}"));
                            let item = self.map_block(&id, b);
                            let item = self.retain(id, item)?;
                            out.push(self.item_event(item));
                        }
                    }
                }
            }
            Some("result") => {
                let turn = self
                    .active
                    .take()
                    .ok_or_else(|| anyhow::anyhow!("claude_unowned_result"))?;
                let status = if self.interrupted {
                    "interrupted"
                } else if event["is_error"] == true {
                    "failed"
                } else {
                    "completed"
                };
                let mut items = vec![
                    json!({"id":format!("user-{turn}"),"type":"userMessage","text":self.user}),
                ];
                items.extend(
                    self.order
                        .iter()
                        .filter_map(|id| self.items.get(id))
                        .cloned(),
                );
                if let Some(text) = event["result"].as_str() {
                    if !items
                        .iter()
                        .any(|i| i["type"] == "agentMessage" && i["text"] == text)
                    {
                        items.push(json!({"id":format!("result-{turn}"),"type":"agentMessage","text":text}));
                    }
                }
                out.push(json!({"method":"turn/completed","params":{"threadId":self.session,"turn":{"id":turn,"status":status,"error":if status=="failed"{json!({"message":"Claude turn failed"})}else{Value::Null},"items":items}}}));
                self.items.clear();
                self.order.clear();
                self.blocks.clear();
            }
            // system idle may precede result: never dispatch another turn here.
            Some("system") => {}
            Some("control_request") => bail!("claude_unexpected_permission_request"),
            _ => {}
        }
        Ok(out)
    }
}

pub async fn line<R: tokio::io::AsyncBufRead + Unpin>(reader: &mut R) -> Result<Option<Value>> {
    use tokio::io::AsyncBufReadExt;
    let mut bytes = vec![];
    let mut deadline = None;
    loop {
        let chunk = if let Some(deadline) = deadline {
            tokio::time::timeout_at(deadline, reader.fill_buf())
                .await
                .map_err(|_| anyhow::anyhow!("claude_frame_timeout"))??
        } else {
            reader.fill_buf().await?
        };
        if deadline.is_none() {
            deadline = Some(tokio::time::Instant::now() + std::time::Duration::from_secs(60));
        }
        if chunk.is_empty() {
            if bytes.is_empty() {
                return Ok(None);
            }
            bail!("claude_truncated_frame");
        }
        let end = chunk
            .iter()
            .position(|b| *b == b'\n')
            .map(|i| i + 1)
            .unwrap_or(chunk.len());
        if bytes.len() + end > 2 * 1024 * 1024 {
            bail!("claude_frame_limit");
        }
        bytes.extend_from_slice(&chunk[..end]);
        reader.consume(end);
        if bytes.last() == Some(&b'\n') {
            return Ok(Some(
                crate::bounded_json::decode(&bytes)
                    .map_err(|_| anyhow::anyhow!("claude_invalid_frame"))?,
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn adapter() -> Adapter {
        Adapter::new("synthetic-session".into()).unwrap()
    }
    fn start(a: &mut Adapter) {
        a.request(
            1,
            "turn/start",
            &json!({"input":[{"type":"text","text":"local user"}]}),
        )
        .unwrap();
    }
    #[test]
    fn idle_is_not_a_result_and_cannot_release_a_second_turn() {
        let mut a = adapter();
        start(&mut a);
        assert!(a
            .consume(json!({"type":"system","subtype":"session_state_changed","state":"idle"}))
            .unwrap()
            .iter()
            .all(|e| e["method"] != "turn/completed"));
        assert!(a.request(2, "turn/start", &json!({"input":[]})).is_err());
        let result = a
            .consume(json!({"type":"result","is_error":false,"result":"answer"}))
            .unwrap();
        assert_eq!(
            result[0]["params"]["turn"]["items"][0]["text"],
            "local user"
        );
        start(&mut a);
    }
    #[test]
    fn tool_result_updates_same_item_and_partial_final_keeps_arrival_order() {
        let mut a = adapter();
        start(&mut a);
        a.consume(json!({"type":"assistant","message":{"id":"z","content":[{"type":"text","text":"first"},{"type":"tool_use","id":"a-tool","name":"Bash","input":{"command":"pwd"}}]}})).unwrap();
        a.consume(json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"a-tool","content":"synthetic output"}]}})).unwrap();
        // Final provider summary repeats the invocation without its result.
        a.consume(json!({"type":"assistant","message":{"id":"z","content":[{"type":"tool_use","id":"a-tool","name":"Bash","input":{"command":"pwd"}}]}})).unwrap();
        let out = a.consume(json!({"type":"result","result":"last"})).unwrap();
        let items = out[0]["params"]["turn"]["items"].as_array().unwrap();
        assert_eq!(items.len(), 4);
        assert_eq!(items[1]["text"], "first");
        assert_eq!(items[2]["id"], "a-tool");
        assert_eq!(items[2]["aggregatedOutput"], "synthetic output");
        assert_eq!(items[2]["status"], "completed");
        assert_eq!(
            items.iter().filter(|i| i["type"] == "userMessage").count(),
            1
        );
    }
    #[test]
    fn identity_and_terminal_conflicts_refuse() {
        let mut a = adapter();
        start(&mut a);
        assert!(a
            .consume(json!({"type":"system","session_id":"wrong"}))
            .is_err());
        assert!(a.consume(json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"unknown"}]}})).is_err());
        a.retain("tool".into(),json!({"id":"tool","type":"commandExecution","command":"first","aggregatedOutput":"done","status":"completed"})).unwrap();
        assert!(a.retain("tool".into(),json!({"id":"tool","type":"commandExecution","command":"changed","status":"inProgress"})).is_err());
    }
    #[tokio::test]
    async fn incomplete_or_duplicate_key_frames_refuse() {
        for bytes in [
            b"{\"type\":\"result\"}".as_slice(),
            b"{\"type\":1,\"type\":2}\n".as_slice(),
        ] {
            assert!(line(&mut tokio::io::BufReader::new(bytes)).await.is_err());
        }
    }
    #[test]
    fn equal_body_final_only_message_does_not_reuse_another_message_block() {
        let mut a = Adapter::new("session".into()).unwrap();
        a.request(1, "turn/start", &json!({"input":[{"type":"text","text":"prompt"}]})).unwrap();
        a.consume(json!({"type":"stream_event","event":{"type":"message_start","message":{"id":"message-a"}}})).unwrap();
        a.consume(json!({"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":"same"}}})).unwrap();
        for id in ["message-a", "message-a", "message-b"] {
            a.consume(json!({"type":"assistant","message":{"id":id,"content":[{"type":"text","text":"same"}]}})).unwrap();
        }
        assert_eq!(a.order, vec!["message-a:0", "message-b:0"]);
        assert_eq!(a.items["message-a:0"]["text"], "same");
        assert_eq!(a.items["message-b:0"]["text"], "same");
    }
    #[test]
    fn supported_import_requires_same_claude_idle_snapshot_and_queue_binding() {
        let base = json!({"version":1,"harness":"claude","agent":"ag_test","cwd":"/synthetic","threadId":"session","queueSha256":"hash","snapshot":{"ready":true,"harness":{"id":"claude"},"runtime":{"state":"idle","error":null},"thread":{"id":"session","status":"idle","activeTurnId":null,"turns":[],"hasOlderItems":true},"queue":[],"backgroundTasks":[],"goal":null,"goalDelivery":null}});
        let mut state = json!({"threadId":"session"});
        import_history(
            &mut state,
            &base,
            "ag_test",
            std::path::Path::new("/synthetic"),
            Some("hash"),
        )
        .unwrap();
        assert_eq!(state["harness"], "claude");
        assert_eq!(state["claudeHistoryStart"], 1);
        let mut receiptless = base.clone();
        receiptless["queueSha256"] = Value::Null;
        import_history(&mut state, &receiptless, "ag_test", std::path::Path::new("/synthetic"), None).unwrap();
        receiptless.as_object_mut().unwrap().remove("queueSha256");
        assert!(import_history(&mut state, &receiptless, "ag_test", std::path::Path::new("/synthetic"), None).is_err());
        let mut faults = vec![];
        for path in [
            "/snapshot/goal",
            "/snapshot/goalDelivery",
            "/snapshot/thread/activeTurnId",
            "/snapshot/runtime/error",
        ] {
            let mut fault = base.clone();
            *fault.pointer_mut(path).unwrap() = json!("unsettled");
            faults.push(fault);
        }
        for (path, value) in [
            ("/queueSha256", json!("other")),
            ("/harness", json!("codex")),
            ("/snapshot/thread/id", json!("other")),
            ("/snapshot/queue", json!([{}])),
            ("/snapshot/backgroundTasks", json!([{}])),
            (
                "/snapshot/thread/turns",
                json!([{"id":"t","status":"inProgress","items":[]}]),
            ),
        ] {
            let mut fault = base.clone();
            *fault.pointer_mut(path).unwrap() = value;
            faults.push(fault);
        }
        let mut fault = base.clone();
        fault["snapshot"].as_object_mut().unwrap().remove("goal");
        faults.push(fault);
        for fault in faults {
            let mut state = json!({"threadId":"session"});
            let before = state.clone();
            assert!(import_history(
                &mut state,
                &fault,
                "ag_test",
                std::path::Path::new("/synthetic"),
                Some("hash")
            )
            .is_err());
            assert_eq!(state, before);
        }
    }
}
