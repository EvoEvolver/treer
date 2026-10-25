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
    models: Vec<Value>,
    aliases: BTreeMap<String, String>,
    current_model: Option<String>,
    model_controls: BTreeMap<u64, String>,
    model_revision: u64,
    input_model_revision: u64,
    settings_epoch: u64,
    input_settings_epoch: Option<u64>,
}
pub enum Request {
    Local(Value),
    Wire(Value),
    Input { wire: Value, dispatched: Value },
    Steer { wire: Value, turn: String, item: Value, after: Option<String> },
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
            models: vec![],
            aliases: BTreeMap::new(),
            current_model: None,
            model_controls: BTreeMap::new(),
            model_revision: 0,
            input_model_revision: 0,
            settings_epoch: 0,
            input_settings_epoch: None,
        })
    }
    pub fn forget_control(&mut self, id: u64) { self.model_controls.remove(&id); }
    pub fn model_revision(&self) -> u64 {self.model_revision}
    // Called only by the waiting RPC future after its exact response wins
    // against the response deadline. Parsing an ACK alone is not commitment.
    pub fn commit_model(&mut self, result: &Value) -> Result<Value> {
        let model=Self::model_name(&result["model"]).ok_or_else(||anyhow::anyhow!("claude_model_ack_invalid"))?;
        Ok(self.observe_model(model)["params"].clone())
    }
    fn model_name(value: &Value) -> Option<&str> {
        value.as_str().filter(|s| !s.is_empty() && s.len() <= 1024 && !s.chars().any(char::is_control))
    }
    fn resolved(&self, model: &str) -> String {
        self.aliases.get(model).cloned().unwrap_or_else(||model.into())
    }
    fn option(alias: &str, model: &str, name: &str, description: &str) -> Value {
        json!({"id":alias,"model":model,"displayName":name,"description":description,"isDefault":alias=="default","hidden":false,"supportedReasoningEfforts":[],"defaultReasoningEffort":null})
    }
    fn observe_model(&mut self, value: &str) -> Value {
        let model = self.resolved(value);
        if !self.models.iter().any(|m|m["model"]==model) && self.models.len()<128 {
            self.models.push(Self::option(&model,&model,&model,"Current model reported by Claude"));
        }
        self.current_model=Some(model.clone());
        self.model_revision+=1;
        json!({"method":"thread/model/updated","params":{"threadId":self.session,"model":model,"modelOptions":self.models,"modelRevision":self.model_revision}})
    }
    fn catalog(&mut self, response: &Value) -> Result<()> {
        self.models.clear();self.aliases.clear();
        if let Some(models)=response.get("models") {
            let models=models.as_array().filter(|m|m.len()<=128).ok_or_else(||anyhow::anyhow!("claude_model_catalog_invalid"))?;
            for info in models {
                let Some(alias)=Self::model_name(&info["value"]).or_else(||Self::model_name(&info["id"])) else {continue;};
                let model=Self::model_name(&info["resolvedModel"]).unwrap_or(alias);
                self.aliases.insert(alias.into(),model.into());
                let option=Self::option(alias,model,Self::model_name(&info["displayName"]).unwrap_or(model),info["description"].as_str().filter(|s|s.len()<=4096).unwrap_or(""));
                if let Some(prior)=self.models.iter_mut().find(|m|m["model"]==model) {
                    if prior["isDefault"]==true {*prior=option;}
                } else {self.models.push(option);}
            }
        }
        if self.current_model.is_none() {self.current_model=self.aliases.get("default").cloned();}
        if let Some(model)=self.current_model.clone() {self.observe_model(&model);}
        Ok(())
    }
    pub fn request(&mut self, id: u64, method: &str, params: &Value) -> Result<Request> {
        Ok(match method {
            "initialize" => {
                self.model_controls.insert(id,String::new());
                Request::Wire(json!({"type":"control_request","request_id":id.to_string(),"request":{"subtype":"initialize"}}))
            },
            "model/list" => Request::Local(json!({"data":self.models})),
            "model/set" => {
                let model=Self::model_name(&params["model"]).ok_or_else(||anyhow::anyhow!("claude_model_invalid"))?;
                // An attempt, including an unacknowledged one, invalidates old
                // input model reports as evidence of the future selection.
                self.settings_epoch+=1;
                self.model_controls.insert(id,model.into());
                Request::Wire(json!({"type":"control_request","request_id":id.to_string(),"request":{"subtype":"set_model","model":model}}))
            },
            "thread/start" | "thread/resume" => {
                if params["threadId"]
                    .as_str()
                    .is_some_and(|s| s != self.session)
                {
                    bail!("claude_session_mismatch");
                }
                Request::Local(
                    json!({"thread":{"id":self.session,"name":"Claude","status":{"type":"idle"},"turns":[]},"model":self.current_model,"modelOptions":self.models,"modelRevision":self.model_revision,"reasoningEffort":null}),
                )
            }
            "turn/start" | "turn/steer" => {
                let steering = method == "turn/steer";
                if params.get("threadId").is_some_and(|id| id != &self.session) || steering && params["threadId"] != self.session {
                    bail!("claude_session_mismatch");
                }
                if steering && (self.active.is_none() || params["expectedTurnId"].as_str() != self.active.as_deref()) {
                    bail!("claude_steer_turn_mismatch");
                }
                if !steering && self.active.is_some() {
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
                let user_id = if steering {
                    uuid::Uuid::new_v4().to_string()
                } else {
                    let turn = uuid::Uuid::new_v4().to_string();
                    let id = format!("user-{turn}");
                    self.active = Some(turn);
                    self.input_model_revision=self.model_revision;
                    self.input_settings_epoch=Some(self.settings_epoch);
                    self.items.clear();
                    self.order.clear();
                    self.blocks.clear();
                    self.interrupted = false;
                    self.announced = false;
                    self.user = text.clone();
                    id
                };
                // Correlated local dispatch result is emitted only after stdin write completes.
                let wire=json!({"type":"user","uuid":user_id,"session_id":self.session,"message":{"role":"user","content":[{"type":"text","text":text}]},"parent_tool_use_id":null});
                if steering {
                    Request::Steer {wire,turn:self.active.clone().unwrap(),item:json!({"id":user_id,"type":"userMessage","text":text}),after:self.order.last().cloned()}
                } else {
                    Request::Input {wire,dispatched:self.dispatched()}
                }
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
    pub fn steered(&mut self, turn: &str, item: Value, after: Option<&str>) -> Result<Value> {
        // A result received before successful write/flush cannot acknowledge
        // this input. It may already have been sent: refuse as UNKNOWN rather
        // than reusing the completed turn, retargeting or retrying the input.
        if self.active.as_deref()!=Some(turn) {bail!("claude_steer_outcome_unknown");}
        let id=item["id"].as_str().ok_or_else(||anyhow::anyhow!("claude_item_identity"))?.to_owned();
        self.retain(id.clone(),item)?;
        self.order.retain(|entry|entry!=&id);
        let position=match after {
            Some(anchor)=>self.order.iter().position(|entry|entry==anchor).map(|i|i+1).ok_or_else(||anyhow::anyhow!("claude_steer_anchor_missing"))?,
            None=>0,
        };
        self.order.insert(position,id);
        Ok(self.dispatched())
    }
    pub fn dispatched(&self) -> Value {
        let mut items=vec![json!({"id":format!("user-{}",self.active.as_deref().unwrap_or("")),"type":"userMessage","text":self.user})];
        items.extend(self.order.iter().filter_map(|id|self.items.get(id)).cloned());
        json!({"turn":{"id":self.active,"status":"inProgress","items":items}})
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
        if event["type"]=="assistant" && self.input_settings_epoch==Some(self.settings_epoch) && self.model_revision<=self.input_model_revision {
            if let Some(model)=Self::model_name(&event["message"]["model"]).filter(|m|*m!="<synthetic>") {
                out.push(self.observe_model(model));
            }
        }
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
                let control=self.model_controls.remove(&id);
                let mut result=r["response"].clone();
                if r["subtype"]=="success" {
                    if let Some(model)=control {
                        if model.is_empty() {self.catalog(&result)?;}
                        else {result=json!({"model":self.resolved(&model)});}
                    }
                }
                out.push(if r["subtype"] == "error" {
                    json!({"id":id,"error":{"code":"claude_control_refused"}})
                } else {
                    json!({"id":id,"result":result})
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
            Some("system") => {
                if event["subtype"]=="init" && ((self.input_settings_epoch.is_none() && self.settings_epoch==0) || (self.input_settings_epoch==Some(self.settings_epoch) && self.model_revision<=self.input_model_revision)) {
                    if let Some(model)=Self::model_name(&event["model"]) {out.push(self.observe_model(model));}
                }
            }
            Some("control_request") => bail!("claude_unexpected_permission_request"),
            _ => {}
        }
        Ok(out)
    }
}

#[cfg(test)]
mod model_tests {
    use super::*;
    fn ack(adapter: &mut Adapter, id: u64, response: Value) -> Vec<Value> {
        adapter.consume(json!({"type":"control_response","response":{"subtype":"success","request_id":id.to_string(),"response":response}})).unwrap()
    }
    #[test]
    fn model_catalog_and_control_scope_do_not_promote_late_old_turn_or_expired_ack() {
        let mut a=Adapter::new("session".into()).unwrap();
        a.request(1,"initialize",&json!({})).unwrap();
        ack(&mut a,1,json!({"models":[{"value":"default","resolvedModel":"sonnet-version"},{"value":"sonnet","resolvedModel":"sonnet-version"},{"value":"opus","resolvedModel":"opus-version"}]}));
        assert_eq!(a.models.len(),2);assert_eq!(a.models[0]["id"],"sonnet");
        a.request(2,"turn/start",&json!({"input":[{"type":"text","text":"A"}]})).unwrap();
        a.request(3,"model/set",&json!({"model":"opus"})).unwrap();
        let result=ack(&mut a,3,json!({}));assert_eq!(result.last().unwrap()["result"]["model"],"opus-version");
        a.commit_model(&result.last().unwrap()["result"]).unwrap();
        let late=a.consume(json!({"type":"assistant","message":{"id":"old-A","model":"sonnet-version","content":[{"type":"text","text":"old model output"}]}})).unwrap();
        assert!(!late.iter().any(|e|e["method"]=="thread/model/updated"));
        assert_eq!(a.current_model.as_deref(),Some("opus-version"));
        a.request(4,"model/set",&json!({"model":"expired"})).unwrap();a.forget_control(4);
        ack(&mut a,4,json!({}));assert_eq!(a.current_model.as_deref(),Some("opus-version"));
        // An ACK parsed in the drop/forget gap still cannot commit a choice:
        // the timed-out RPC future never consumes this candidate result.
        a.request(6,"model/set",&json!({"model":"gap"})).unwrap();
        let candidate=ack(&mut a,6,json!({}));assert_eq!(candidate.last().unwrap()["result"]["model"],"gap");
        a.forget_control(6);assert_eq!(a.current_model.as_deref(),Some("opus-version"));
        a.consume(json!({"type":"result","subtype":"success","is_error":false})).unwrap();
        let stale=a.consume(json!({"type":"system","subtype":"init","model":"old-after-timeout"})).unwrap();
        assert!(!stale.iter().any(|e|e["method"]=="thread/model/updated"));
        assert_eq!(a.current_model.as_deref(),Some("opus-version"));
        a.request(5,"turn/start",&json!({"input":[{"type":"text","text":"B"}]})).unwrap();
        let fresh=a.consume(json!({"type":"system","subtype":"init","model":"reported-actual"})).unwrap();
        assert!(fresh.iter().any(|e|e["params"]["model"]=="reported-actual"));
        assert_eq!(a.current_model.as_deref(),Some("reported-actual"));
    }
    #[test]
    fn malformed_catalog_refuses_boundedly_and_invalid_model_never_emits_control() {
        let mut a=Adapter::new("session".into()).unwrap();
        assert!(a.request(2,"model/set",&json!({"model":"bad\nmodel"})).is_err());
        a.request(1,"initialize",&json!({})).unwrap();
        assert!(a.consume(json!({"type":"control_response","response":{"subtype":"success","request_id":"1","response":{"models":{}}}})).is_err());
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
    fn steering_is_exact_turn_input_not_interrupt_or_new_turn() {
        let mut a=adapter();start(&mut a);
        let turn=a.active.clone().unwrap();
        a.consume(json!({"type":"assistant","message":{"id":"before","content":[{"type":"text","text":"before steer"}]}})).unwrap();
        for id in [2,3] {
            let request=a.request(id,"turn/steer",&json!({"threadId":"synthetic-session","expectedTurnId":turn,"input":[{"type":"text","text":"same steer"}]})).unwrap();
            let Request::Steer {wire,turn:expected,item,after}=request else {panic!("expected native steer")};
            let dispatched=a.steered(&expected,item,after.as_deref()).unwrap();
            assert_eq!(wire["type"],"user");assert!(wire["uuid"].is_string());
            assert_eq!(wire["message"]["content"][0]["text"],"same steer");
            assert_eq!(dispatched["turn"]["id"],turn);
        }
        assert!(a.request(4,"turn/steer",&json!({"threadId":"wrong","expectedTurnId":turn,"input":[]})).is_err());
        assert!(a.request(5,"turn/steer",&json!({"threadId":"synthetic-session","expectedTurnId":"wrong","input":[]})).is_err());
        let final_events=a.consume(json!({"type":"result","result":"done"})).unwrap();
        let items=final_events.last().unwrap()["params"]["turn"]["items"].as_array().unwrap();
        assert_eq!(items.iter().map(|i|i["text"].as_str().unwrap()).collect::<Vec<_>>(),["local user","before steer","same steer","same steer","done"]);
        assert_ne!(items[2]["id"],items[3]["id"]);
        assert!(a.request(6,"turn/steer",&json!({"threadId":"synthetic-session","expectedTurnId":turn,"input":[]})).is_err());
    }
    #[test]
    fn result_during_steer_write_is_unknown_not_old_turn_completion() {
        let mut a=adapter();start(&mut a);
        let turn=a.active.clone().unwrap();
        let Request::Steer {turn:expected,item,after,..}=a.request(2,"turn/steer",&json!({"threadId":"synthetic-session","expectedTurnId":turn,"input":[{"type":"text","text":"not acknowledged"}]})).unwrap() else {panic!()};
        let completed=a.consume(json!({"type":"result","result":"old turn result"})).unwrap();
        let items=completed.last().unwrap()["params"]["turn"]["items"].as_array().unwrap();
        assert!(!items.iter().any(|i|i["text"]=="not acknowledged"));
        assert_eq!(a.steered(&expected,item,after.as_deref()).unwrap_err().to_string(),"claude_steer_outcome_unknown");
        assert!(a.active.is_none());
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
