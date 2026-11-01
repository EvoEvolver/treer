//! Claude stream-json bridge. Session identity is never inferred from private files.
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

pub struct Adapter {
    session: String,
    active: Option<String>,
    message: String,
    blocks: BTreeMap<u64, (String, Value, String)>,
    items: BTreeMap<String, Value>,
    // Serialized size of each retained item, so a stream delta costs O(item)
    // instead of re-serializing the whole turn.
    sizes: BTreeMap<String, usize>,
    order: Vec<String>,
    user: String,
    interrupted: bool,
    pending_interrupts: BTreeMap<u64, (String, u64)>,
    invocation_epoch: u64,
    announced: bool,
    models: Vec<Value>,
    aliases: BTreeMap<String, String>,
    current_model: Option<String>,
    model_controls: BTreeMap<u64, String>,
    effort_controls: BTreeMap<u64, Value>,
    effort_supported: bool,
    current_effort: Value,
    effort_known: bool,
    effort_revision: u64,
    model_revision: u64,
    input_model_revision: u64,
    settings_epoch: u64,
    input_settings_epoch: Option<u64>,
    goals_supported: bool,
    task_runs: BTreeMap<String, bool>,
    task_followup: Option<Box<Adapter>>,
    input_commands: BTreeSet<String>,
    native_commands: BTreeSet<String>,
    native_command: Option<String>,
    scheduled_result: Option<Value>,
    active_input: Option<String>,
    // A task invocation opened only to display output that no tracked
    // invocation owns (unknown task run, restart, ...). Display-only.
    orphan: bool,
    // The last turn this adapter completed: late output that belongs to no
    // open turn is attached there instead of being dropped.
    last_turn: Option<String>,
}
pub enum Request {
    Local(Value),
    Wire(Value),
    Input {
        wire: Value,
        dispatched: Value,
    },
    Steer {
        wire: Value,
        turn: String,
        epoch: u64,
        item: Value,
        after: Option<String>,
    },
}
const IMAGE_BYTES: usize = 24 * 1024 * 1024;
/// Display-journal cap for one tool output. The journal is a display cache;
/// the Claude transcript stays the source of truth for the full text.
const DISPLAY_OUTPUT_BYTES: usize = 32 * 1024;
/// Display cap for one string inside a tool's input or a file diff.
const DISPLAY_INPUT_BYTES: usize = 16 * 1024;
/// Per-turn display budget; past it items are shrunk, not refused.
const TURN_BYTES: usize = 4 * 1024 * 1024;
/// Keep the head and tail of `text` within about `limit` bytes.
pub(crate) fn display_text(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.into();
    }
    let mut head = limit * 3 / 4;
    while !text.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail = text.len() - limit / 4;
    while !text.is_char_boundary(tail) {
        tail += 1;
    }
    format!(
        "{}\n… [{} bytes not kept in the display journal; the Claude transcript has the full text] …\n{}",
        &text[..head],
        tail - head,
        &text[tail..]
    )
}
/// Cap every string in `value` (tool input, structured output) for display.
fn display_value(value: &Value, limit: usize) -> Value {
    match value {
        Value::String(text) if text.len() > limit => json!(display_text(text, limit)),
        Value::Array(values) => {
            Value::Array(values.iter().map(|v| display_value(v, limit)).collect())
        }
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), display_value(v, limit)))
                .collect(),
        ),
        other => other.clone(),
    }
}
/// A tool_result's display text: text blocks joined, other blocks (images)
/// named, never their payload.
fn tool_result_text(content: &Value) -> String {
    let text = match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .map(|b| match b["text"].as_str() {
                Some(text) if b["type"] == "text" => text.to_owned(),
                _ => format!("[{}]", b["type"].as_str().unwrap_or("content")),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    };
    display_text(&text, DISPLAY_OUTPUT_BYTES)
}
const IMAGE_BASE64_BYTES: usize = 32 * 1024 * 1024;
fn image_source(source: &Value) -> Result<usize> {
    use base64::Engine;
    let data = source["data"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("claude_image_input_invalid"))?;
    if source["type"] != "base64"
        || data.is_empty()
        || data.len() > crate::image_format::FILE_LIMIT.div_ceil(3) * 4
    {
        bail!("claude_image_input_invalid");
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|_| anyhow::anyhow!("claude_image_input_invalid"))?;
    if crate::image_format::format(&bytes)?.0 != source["media_type"] {
        bail!("claude_image_input_invalid");
    }
    Ok(bytes.len())
}
impl Adapter {
    // Only fixed adapter codes and counts cross into public diagnostics. Never
    // include the refused frame, error context, IDs, content or provider text.
    pub fn refusal_diagnostic(&self, error: &anyhow::Error) -> Value {
        let message = error.to_string();
        let code = match message.as_str() {
            code @ ("claude_session_mismatch"
            | "claude_task_session_missing"
            | "claude_task_result_origin"
            | "claude_task_result_missing"
            | "claude_task_input_interleaving"
            | "claude_unowned_task_result"
            | "claude_task_run_limit"
            | "claude_control_response_kind"
            | "claude_control_identity"
            | "claude_unowned_stream"
            | "claude_unowned_assistant"
            | "claude_unowned_result"
            | "claude_unknown_tool_result"
            | "claude_tool_id"
            | "claude_message_id"
            | "claude_block_identity"
            | "claude_block_limit"
            | "claude_tool_input_limit"
            | "claude_item_identity"
            | "claude_item_limit"
            | "claude_model_catalog_invalid"
            | "claude_terminal_conflict"
            | "claude_turn_limit"
            | "claude_command_identity"
            | "claude_command_session"
            | "claude_command_interleaving"
            | "claude_command_limit"
            | "claude_command_state"
            | "claude_command_incomplete"
            | "claude_invocation_limit") => code,
            _ => "claude_protocol_refused",
        };
        json!({"code":code,"activeInput":self.active.is_some(),
            "taskFollowup":self.task_followup.is_some(),"taskRuns":self.task_runs.len()})
    }
    // Current process state only. This says nothing about historical effects
    // and must never be used as terminal receipt/history authority.
    pub fn idle_for_disposition(&self) -> bool {
        self.active.is_none()
            && self.pending_interrupts.is_empty()
            && self.task_followup.is_none()
            && self.native_command.is_none()
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
            sizes: BTreeMap::new(),
            order: vec![],
            user: String::new(),
            interrupted: false,
            pending_interrupts: BTreeMap::new(),
            invocation_epoch: 0,
            announced: false,
            models: vec![],
            aliases: BTreeMap::new(),
            current_model: None,
            model_controls: BTreeMap::new(),
            effort_controls: BTreeMap::new(),
            effort_supported: false,
            current_effort: Value::Null,
            effort_known: false,
            effort_revision: 0,
            model_revision: 0,
            input_model_revision: 0,
            settings_epoch: 0,
            input_settings_epoch: None,
            goals_supported: false,
            task_runs: BTreeMap::new(),
            task_followup: None,
            input_commands: BTreeSet::new(),
            native_commands: BTreeSet::new(),
            native_command: None,
            scheduled_result: None,
            active_input: None,
            orphan: false,
            last_turn: None,
        })
    }
    /// After a frame the adapter could not place, drop any background
    /// invocation scope it was tracking so the stream keeps flowing. Output
    /// that scope already buffered is returned as display-only task output
    /// rather than discarded.
    pub fn skip_refused_frame(&mut self) -> Vec<Value> {
        self.native_command = None;
        self.scheduled_result = None;
        self.salvage_followup().into_iter().collect()
    }
    fn salvage_followup(&mut self) -> Option<Value> {
        let followup = self.task_followup.take()?;
        let items: Vec<Value> = followup
            .order
            .iter()
            .filter_map(|id| followup.items.get(id))
            .cloned()
            .collect();
        if items.is_empty() {
            return None;
        }
        let id = followup.active.clone()?;
        Some(
            json!({"method":"thread/claude-task-output","params":{"threadId":self.session,"turn":{"id":id,"status":"interrupted","error":null,"items":items,"projectionOnly":true,"taskRunId":null}}}),
        )
    }
    pub fn forget_control(&mut self, id: u64) {
        self.model_controls.remove(&id);
        self.effort_controls.remove(&id);
    }
    pub fn commit_effort(&mut self, result: &Value) -> Result<Value> {
        if result["unsupported"] == true {
            return Ok(result.clone());
        }
        if result["probe"] == true {
            self.effort_supported = true;
        } else {
            if !result
                .get("effort")
                .is_some_and(|e| e.is_null() || Self::effort(e).is_some())
            {
                bail!("claude_effort_ack_invalid");
            }
            self.current_effort = result["effort"].clone();
            self.effort_known = true;
            // Invalidate already-queued selection reports captured before ACK.
            self.effort_revision += 1;
        }
        Ok(
            json!({"reasoningEffort":self.current_effort,"effortSelectionStatus":self.effort_status(),"effortRevision":self.effort_revision,"modelOptions":self.visible_models()}),
        )
    }
    fn effort_status(&self) -> &'static str {
        if self.effort_known {
            "confirmed"
        } else {
            "unknown"
        }
    }
    pub fn unknown_effort(&mut self) {
        self.current_effort = Value::Null;
        self.effort_known = false;
        self.effort_revision += 1;
    }
    pub fn effort_revision(&self) -> u64 {
        self.effort_revision
    }
    fn effort(value: &Value) -> Option<String> {
        let value = value.as_str()?.trim().to_ascii_lowercase();
        matches!(
            value.as_str(),
            "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max" | "ultra"
        )
        .then_some(value)
    }
    fn visible_models(&self) -> Vec<Value> {
        self.models
            .iter()
            .map(|model| {
                let mut model = model.clone();
                if !self.effort_supported {
                    model["supportedReasoningEfforts"] = json!([]);
                }
                model
            })
            .collect()
    }
    fn validate_settings(&self, params: &Value) -> Result<()> {
        if let Some(effort) = params.get("reasoningEffort") {
            let model = match params.get("model") {
                Some(model) => model.as_str().map(|s| self.resolved(s)),
                None => self.current_model.clone(),
            };
            if !self.effort_supported
                || !effort.is_null()
                    && !self.models.iter().any(|m| {
                        Some(m["model"].as_str().unwrap_or_default()) == model.as_deref()
                            && m["supportedReasoningEfforts"]
                                .as_array()
                                .is_some_and(|levels| {
                                    levels.iter().any(|e| e["reasoningEffort"] == *effort)
                                })
                    })
            {
                bail!("claude_reasoning_effort_not_supported");
            }
        }
        Ok(())
    }
    pub fn model_revision(&self) -> u64 {
        self.model_revision
    }
    // Called only by the waiting RPC future after its exact response wins
    // against the response deadline. Parsing an ACK alone is not commitment.
    pub fn commit_model(&mut self, result: &Value) -> Result<Value> {
        let model = Self::model_name(&result["model"])
            .ok_or_else(|| anyhow::anyhow!("claude_model_ack_invalid"))?;
        Ok(self.observe_model(model)["params"].clone())
    }
    fn model_name(value: &Value) -> Option<&str> {
        value
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 1024 && !s.chars().any(char::is_control))
    }
    fn resolved(&self, model: &str) -> String {
        self.aliases
            .get(model)
            .cloned()
            .unwrap_or_else(|| model.into())
    }
    fn option(alias: &str, model: &str, name: &str, description: &str) -> Value {
        json!({"id":alias,"model":model,"displayName":name,"description":description,"isDefault":alias=="default","hidden":false,"supportedReasoningEfforts":[],"defaultReasoningEffort":null})
    }
    fn observe_model(&mut self, value: &str) -> Value {
        let model = self.resolved(value);
        if !self.current_effort.is_null()
            && !self.models.iter().any(|m| {
                m["model"] == model
                    && m["supportedReasoningEfforts"]
                        .as_array()
                        .is_some_and(|levels| {
                            levels
                                .iter()
                                .any(|e| e["reasoningEffort"] == self.current_effort)
                        })
            })
        {
            self.unknown_effort();
        }
        if !self.models.iter().any(|m| m["model"] == model) && self.models.len() < 128 {
            self.models.push(Self::option(
                &model,
                &model,
                &model,
                "Current model reported by Claude",
            ));
        }
        self.current_model = Some(model.clone());
        self.model_revision += 1;
        json!({"method":"thread/model/updated","params":{"threadId":self.session,"model":model,"modelOptions":self.visible_models(),"modelRevision":self.model_revision,"reasoningEffort":self.current_effort,"effortSelectionStatus":self.effort_status(),"effortRevision":self.effort_revision}})
    }
    fn catalog(&mut self, response: &Value) -> Result<()> {
        self.goals_supported = crate::claude_goal::supported(response);
        self.models.clear();
        self.aliases.clear();
        if let Some(models) = response.get("models") {
            let models = models
                .as_array()
                .filter(|m| m.len() <= 128)
                .ok_or_else(|| anyhow::anyhow!("claude_model_catalog_invalid"))?;
            for info in models {
                let Some(alias) =
                    Self::model_name(&info["value"]).or_else(|| Self::model_name(&info["id"]))
                else {
                    continue;
                };
                let model = Self::model_name(&info["resolvedModel"]).unwrap_or(alias);
                self.aliases.insert(alias.into(), model.into());
                let mut option = Self::option(
                    alias,
                    model,
                    Self::model_name(&info["displayName"]).unwrap_or(model),
                    info["description"]
                        .as_str()
                        .filter(|s| s.len() <= 4096)
                        .unwrap_or(""),
                );
                if info["supportsEffort"] == true {
                    let mut levels = Vec::new();
                    for value in info["supportedEffortLevels"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .take(32)
                    {
                        if let Some(value) = Self::effort(value) {
                            let entry = json!({"reasoningEffort":value,"description":""});
                            if !levels.contains(&entry) {
                                levels.push(entry);
                            }
                        }
                    }
                    option["supportedReasoningEfforts"] = json!(levels);
                }
                if let Some(prior) = self.models.iter_mut().find(|m| m["model"] == model) {
                    if prior["isDefault"] == true {
                        *prior = option;
                    }
                } else {
                    self.models.push(option);
                }
            }
        }
        if self.current_model.is_none() {
            self.current_model = self.aliases.get("default").cloned();
        }
        if let Some(model) = self.current_model.clone() {
            self.observe_model(&model);
        }
        Ok(())
    }
    pub fn request(&mut self, id: u64, method: &str, params: &Value) -> Result<Request> {
        Ok(match method {
            "initialize" => {
                self.model_controls.insert(id, String::new());
                Request::Wire(
                    json!({"type":"control_request","request_id":id.to_string(),"request":{"subtype":"initialize"}}),
                )
            }
            "model/list" => Request::Local(json!({"data":self.visible_models()})),
            "settings/validate" => {
                self.validate_settings(params)?;
                Request::Local(json!({}))
            }
            "effort/probe" | "effort/set" => {
                let probe = method == "effort/probe";
                if probe
                    && !self.models.iter().any(|m| {
                        m["supportedReasoningEfforts"]
                            .as_array()
                            .is_some_and(|e| !e.is_empty())
                    })
                {
                    return Ok(Request::Local(json!({"unsupported":true})));
                }
                if !probe {
                    self.validate_settings(params)?;
                    self.settings_epoch += 1;
                }
                self.effort_controls.insert(
                    id,
                    if probe {
                        json!({"probe":true})
                    } else {
                        json!({"effort":params["reasoningEffort"]})
                    },
                );
                let settings = if probe {
                    json!({})
                } else {
                    json!({"effortLevel":params["reasoningEffort"]})
                };
                Request::Wire(
                    json!({"type":"control_request","request_id":id.to_string(),"request":{"subtype":"apply_flag_settings","settings":settings}}),
                )
            }
            "thread/goal/get" => Request::Local(
                json!({"goal":null,"supported":self.goals_supported,"confirmed":false}),
            ),
            "model/set" => {
                let model = Self::model_name(&params["model"])
                    .ok_or_else(|| anyhow::anyhow!("claude_model_invalid"))?;
                // An attempt, including an unacknowledged one, invalidates old
                // input model reports as evidence of the future selection.
                self.settings_epoch += 1;
                self.model_controls.insert(id, model.into());
                Request::Wire(
                    json!({"type":"control_request","request_id":id.to_string(),"request":{"subtype":"set_model","model":model}}),
                )
            }
            "thread/start" | "thread/resume" => {
                if params["threadId"]
                    .as_str()
                    .is_some_and(|s| s != self.session)
                {
                    bail!("claude_session_mismatch");
                }
                Request::Local(
                    json!({"thread":{"id":self.session,"name":"Claude","status":{"type":"idle"},"turns":[]},"model":self.current_model,"modelOptions":self.visible_models(),"modelRevision":self.model_revision,"reasoningEffort":self.current_effort,"effortSelectionStatus":self.effort_status(),"effortRevision":self.effort_revision}),
                )
            }
            "turn/start" | "turn/steer" => {
                let steering = method == "turn/steer";
                if steering && (self.task_followup.is_some() || self.native_command.is_some()) {
                    bail!("claude_steer_native_invocation_busy");
                }
                if params.get("threadId").is_some_and(|id| id != &self.session)
                    || steering && params["threadId"] != self.session
                {
                    bail!("claude_session_mismatch");
                }
                if steering
                    && (self.active.is_none()
                        || params["expectedTurnId"].as_str() != self.active.as_deref())
                {
                    bail!("claude_steer_turn_mismatch");
                }
                if !steering && self.active.is_some() {
                    bail!("claude_concurrent_turn");
                }
                let inputs = params["input"]
                    .as_array()
                    .ok_or_else(|| anyhow::anyhow!("claude_input"))?;
                let mut content = Vec::new();
                let mut image_count = 0;
                let mut image_bytes = 0;
                for input in inputs {
                    match input["type"].as_str() {
                        Some("text") => {
                            let text = input["text"]
                                .as_str()
                                .ok_or_else(|| anyhow::anyhow!("claude_input_invalid"))?;
                            content.push(json!({"type":"text","text":text}));
                        }
                        Some("image") => {
                            let source = &input["source"];
                            image_count += 1;
                            image_bytes += image_source(source)?;
                            if image_count > crate::image_format::COUNT_LIMIT
                                || image_bytes > IMAGE_BYTES
                            {
                                bail!("claude_image_input_invalid");
                            }
                            let data = source["data"].as_str().unwrap();
                            content.push(json!({"type":"image","source":{"type":"base64","media_type":source["media_type"],"data":data}}));
                        }
                        _ => bail!("claude_input_invalid"),
                    }
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
                    self.input_model_revision = self.model_revision;
                    self.input_settings_epoch = Some(self.settings_epoch);
                    self.items.clear();
                    self.sizes.clear();
                    self.order.clear();
                    self.blocks.clear();
                    self.interrupted = false;
                    self.announced = false;
                    self.user = text.clone();
                    self.active_input = Some(id.clone());
                    id
                };
                // Correlated local dispatch result is emitted only after stdin write completes.
                // Register before releasing the adapter lock for write: a
                // producer command/start may race that write's return.
                if self.input_commands.len() >= 1024 {
                    bail!("claude_command_limit");
                }
                self.input_commands.insert(user_id.clone());
                let wire = json!({"type":"user","uuid":user_id,"session_id":self.session,"message":{"role":"user","content":content},"parent_tool_use_id":null});
                if steering {
                    Request::Steer {
                        wire,
                        turn: self.active.clone().unwrap(),
                        epoch: self.invocation_epoch,
                        item: json!({"id":user_id,"type":"userMessage","text":text}),
                        after: self.order.last().cloned(),
                    }
                } else {
                    Request::Input {
                        wire,
                        dispatched: self.dispatched(),
                    }
                }
            }
            "turn/interrupt" => {
                if self.task_followup.is_some() || self.native_command.is_some() {
                    bail!("claude_interrupt_task_invocation_busy");
                }
                let turn = self
                    .active
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("claude_no_active_turn"))?;
                self.pending_interrupts
                    .insert(id, (turn, self.invocation_epoch));
                Request::Wire(
                    json!({"type":"control_request","request_id":id.to_string(),"request":{"subtype":"interrupt"}}),
                )
            }
            _ => bail!("claude_method_unsupported"),
        })
    }
    pub fn steered(
        &mut self,
        turn: &str,
        epoch: u64,
        item: Value,
        after: Option<&str>,
    ) -> Result<Value> {
        // The turn ended while this input was being written. Claude runs
        // queued input as its next invocation, so the input starts a new turn.
        if self.active.is_none()
            && self.invocation_epoch == epoch
            && self.task_followup.is_none()
            && self.native_command.is_none()
        {
            let text = item["text"].as_str().unwrap_or("").to_owned();
            self.active = Some(uuid::Uuid::new_v4().to_string());
            self.input_model_revision = self.model_revision;
            self.input_settings_epoch = Some(self.settings_epoch);
            self.items.clear();
            self.sizes.clear();
            self.order.clear();
            self.blocks.clear();
            self.interrupted = false;
            self.announced = false;
            self.user = text;
            self.active_input = item["id"].as_str().map(str::to_owned);
            return Ok(self.dispatched());
        }
        // A background invocation started meanwhile: the input is not tracked.
        if self.active.as_deref() != Some(turn)
            || self.invocation_epoch != epoch
            || self.task_followup.is_some()
            || self.native_command.is_some()
        {
            bail!("claude_steer_outcome_unknown");
        }
        let id = item["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("claude_item_identity"))?
            .to_owned();
        self.retain(id.clone(), item)?;
        self.order.retain(|entry| entry != &id);
        let position = match after {
            Some(anchor) => self
                .order
                .iter()
                .position(|entry| entry == anchor)
                .map(|i| i + 1)
                .ok_or_else(|| anyhow::anyhow!("claude_steer_anchor_missing"))?,
            None => 0,
        };
        self.order.insert(position, id);
        Ok(self.dispatched())
    }
    pub fn dispatched(&self) -> Value {
        let mut items = vec![
            json!({"id":format!("user-{}",self.active.as_deref().unwrap_or("")),"type":"userMessage","text":self.user}),
        ];
        items.extend(
            self.order
                .iter()
                .filter_map(|id| self.items.get(id))
                .cloned(),
        );
        json!({"turn":{"id":self.active,"status":"inProgress","items":items}})
    }
    fn stamped_input(&self, event: &Value) -> bool {
        let owned = |id: &Value| {
            id.as_str().is_some_and(|id| {
                self.active_input.as_deref() == Some(id) || self.input_commands.contains(id)
            })
        };
        owned(&event["user_message_uuid"])
            || event["user_message_uuids"]
                .as_array()
                .is_some_and(|ids| ids.iter().any(owned))
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
                    json!({"id":id,"type":"commandExecution","command":display_value(&b["input"]["command"], DISPLAY_INPUT_BYTES),"aggregatedOutput":"","status":"inProgress"})
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
                            json!({"path":path,"kind":if add{"add"}else{"update"},"diff":display_text(&diff, DISPLAY_OUTPUT_BYTES)})
                        })
                        .collect::<Vec<_>>();
                    json!({"id":id,"type":"fileChange","status":"inProgress","changes":changes})
                }
                _ => {
                    json!({"id":id,"type":"claudeToolCall","tool":b["name"],"input":display_value(&b["input"], DISPLAY_INPUT_BYTES),"status":"inProgress"})
                }
            },
            _ => json!({"id":id,"type":"other"}),
        }
    }
    fn retain(&mut self, id: String, mut item: Value) -> Result<Value> {
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
        // Only the changed item is serialized; the others' sizes are cached.
        let others = self
            .items
            .iter()
            .filter(|(key, _)| *key != &id)
            .map(|(key, value)| {
                self.sizes
                    .get(key)
                    .copied()
                    .unwrap_or_else(|| value.to_string().len())
            })
            .sum::<usize>();
        let mut size = item.to_string().len();
        if others + size > TURN_BYTES / 4 * 3 {
            // Past the soft budget, degrade instead of refusing the frame:
            // keep a short display form of this item (the last quarter of
            // the budget is headroom for these). The Claude transcript keeps
            // the full text.
            for key in [
                "aggregatedOutput",
                "output",
                "input",
                "changes",
                "text",
                "command",
            ] {
                if let Some(value) = item.get(key) {
                    item[key] = display_value(value, 1024);
                }
            }
            size = item.to_string().len();
            if others + size > TURN_BYTES {
                bail!("claude_turn_limit");
            }
        }
        if !self.items.contains_key(&id) {
            self.order.push(id.clone());
        }
        self.sizes.insert(id.clone(), size);
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
        // Claude's scheduler emits a command/start -> init -> output/result ->
        // matching command/completed sequence, with NO user UUID or result
        // origin. Only this explicit wire scope can admit such output. Never
        // borrow the waiting user input's turn or receipt for it.
        if event["type"] == "command_lifecycle" {
            let id = event["command_uuid"]
                .as_str()
                .filter(|s| !s.is_empty() && s.len() <= 512)
                .ok_or_else(|| anyhow::anyhow!("claude_command_identity"))?;
            if event["session_id"] != self.session {
                bail!("claude_command_session");
            }
            if event["state"] == "queued" {
                return Ok(vec![]);
            }
            if self.input_commands.contains(id) {
                if self.native_command.is_some() {
                    bail!("claude_command_interleaving");
                }
                if event["state"] == "completed" {
                    self.input_commands.remove(id);
                } else if event["state"] != "started" {
                    bail!("claude_command_state");
                }
                return Ok(vec![]);
            }
            if event["state"] == "started" {
                if uuid::Uuid::parse_str(id).is_err()
                    || self.native_command.is_some()
                    || self.task_followup.is_some()
                    || self.native_commands.contains(id)
                {
                    bail!("claude_command_interleaving");
                }
                if self.native_commands.len() >= 1024 {
                    bail!("claude_command_limit");
                }
                self.native_commands.insert(id.into());
                self.native_command = Some(id.into());
                self.invocation_epoch = self
                    .invocation_epoch
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("claude_invocation_limit"))?;
            } else if event["state"] == "completed" && self.native_command.as_deref() == Some(id) {
                if self.task_followup.is_some() {
                    bail!("claude_command_incomplete");
                }
                self.native_command = None;
                if let Some(turn) = self.scheduled_result.take() {
                    return Ok(vec![
                        json!({"method":"thread/claude-scheduled-output","params":{"threadId":self.session,"turn":turn}}),
                    ]);
                }
            } else if event["state"] == "completed" && self.native_command.is_none() {
                // A normal result retires local IDs even on older producers
                // without lifecycle events. A later completion has no authority.
                return Ok(vec![]);
            } else {
                bail!("claude_command_state");
            }
            return Ok(vec![]);
        }
        // Native task completion can start its own model invocation AFTER the
        // submitted input's result. Its result has task origin, not an input
        // UUID. Keep all of that invocation separate, even if a new local
        // input has already been written and is waiting in Claude's queue.
        // Sub-agent frames (parent_tool_use_id set) are nested output. They
        // never start, own or close an invocation and never touch the main
        // message's stream state. Claude sends only their tool calls and
        // results; partial stream events are skipped outright.
        let parent = event["parent_tool_use_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        if parent.is_some() && event["type"] == "stream_event" {
            return Ok(vec![]);
        }
        let task_init = event["type"] == "system"
            && event["subtype"] == "init"
            && event.get("user_message_uuids").is_none()
            && (self.task_runs.values().any(|consumed| !consumed) || self.native_command.is_some());
        // Claude may report the task completion after that invocation's init.
        // Its first output then still belongs to the task, not to nobody.
        let late_task = matches!(event["type"].as_str(), Some("stream_event" | "assistant"))
            && parent.is_none()
            && self.active.is_none()
            && self.native_command.is_none()
            && self.task_runs.values().any(|consumed| !consumed);
        // Model output nobody tracks (an invocation whose task run was never
        // announced, output after a restart, ...) is still the user's
        // visible output: show it as display-only task output, never drop it.
        let orphan = !late_task
            && parent.is_none()
            && event["is_meta"] != true
            && event["isReplay"] != true
            && self.active.is_none()
            && self.native_command.is_none()
            && self.task_followup.is_none()
            && (event["type"] == "assistant"
                || event["type"] == "stream_event" && event["event"]["type"] == "message_start");
        if (task_init || late_task || orphan) && self.task_followup.is_none() {
            if self.scheduled_result.is_some() {
                bail!("claude_command_incomplete");
            }
            if event["session_id"] != self.session {
                bail!("claude_task_session_missing");
            }
            let mut followup = Self::new(self.session.clone())?;
            self.invocation_epoch = self
                .invocation_epoch
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("claude_invocation_limit"))?;
            followup.active = Some(uuid::Uuid::new_v4().to_string());
            followup.orphan = orphan;
            self.task_followup = Some(Box::new(followup));
            if task_init {
                return Ok(vec![]);
            }
        }
        // Claude folds a queued input into the running task invocation
        // ("absorbed_mid_turn") and stamps the replies with its uuid. From
        // then on the invocation answers that input: its task output so far
        // becomes the start of the input's turn, and its result closes it.
        let stamped = self.active.is_some() && self.stamped_input(&event);
        let mut folded = vec![];
        if stamped {
            if let Some(followup) = self.task_followup.take() {
                let followup = *followup;
                self.order.retain(|id| !followup.items.contains_key(id));
                self.order.splice(0..0, followup.order);
                for (id, item) in followup.items {
                    if !self.items.contains_key(&id) {
                        self.sizes.insert(id.clone(), item.to_string().len());
                        self.items.insert(id, item);
                    }
                }
                if self.blocks.is_empty() {
                    self.message = followup.message;
                    self.blocks = followup.blocks;
                }
                folded = self
                    .order
                    .iter()
                    .filter_map(|id| self.items.get(id))
                    .cloned()
                    .collect();
            }
        }
        if self.task_followup.is_some()
            && matches!(
                event["type"].as_str(),
                Some("stream_event" | "assistant" | "user" | "result")
            )
        {
            let final_result = event["type"] == "result";
            let run = event["origin"]["runId"].as_str().unwrap_or("").to_owned();
            let scheduled = event.get("origin").is_none() && self.native_command.is_some();
            let orphan = self.task_followup.as_ref().is_some_and(|f| f.orphan);
            if final_result
                && (event["session_id"] != self.session
                    || (!scheduled
                        && !orphan
                        && (event["origin"]["kind"] != "task-notification"
                            || event["origin"]["producer"] != "session-task"
                            || self.task_runs.get(&run) != Some(&false)))
                    || event.get("user_message_uuids").is_some()
                    || event.get("user_message_uuid").is_some())
            {
                bail!("claude_task_result_origin");
            }
            // Origin admission belongs to this outer adapter. The inner
            // transcript mapper must not interpret it as a second invocation.
            let mut mapped_event = event;
            if final_result {
                mapped_event.as_object_mut().unwrap().remove("origin");
            }
            let messages = self.task_followup.as_mut().unwrap().consume(mapped_event)?;
            // Partial content is bounded by the ordinary per-turn adapter
            // limits. Publish only a complete origin-validated display turn.
            if final_result {
                if !scheduled && (!orphan || self.task_runs.contains_key(&run)) {
                    self.task_runs.insert(run.clone(), true);
                }
                self.task_followup = None;
                let mut turn = messages
                    .into_iter()
                    .find(|m| m["method"] == "turn/completed")
                    .ok_or_else(|| anyhow::anyhow!("claude_task_result_missing"))?["params"]
                    ["turn"]
                    .clone();
                turn["items"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|i| i["type"] != "userMessage");
                turn["projectionOnly"] = json!(true);
                if scheduled {
                    turn["nativeCommandId"] = json!(self.native_command);
                    self.scheduled_result = Some(turn);
                    return Ok(vec![]);
                }
                turn["taskRunId"] = if run.is_empty() {
                    Value::Null
                } else {
                    json!(run)
                };
                return Ok(vec![
                    json!({"method":"thread/claude-task-output","params":{"threadId":self.session,"turn":turn}}),
                ]);
            }
            return Ok(vec![]);
        }
        if self.task_followup.is_some() && event["type"] == "system" && event["subtype"] == "init" {
            // A repeated init of the same task invocation carries no input.
            if event.get("user_message_uuids").is_none() {
                return Ok(vec![]);
            }
            // A new input invocation began without the task's result: keep
            // the task's partial output and continue with the input.
            if self.native_command.is_some() {
                bail!("claude_task_input_interleaving");
            }
            let salvaged = self.salvage_followup();
            let mut out = self.consume(event)?;
            out.splice(0..0, salvaged);
            return Ok(out);
        }
        if self.native_command.is_some()
            && (matches!(
                event["type"].as_str(),
                Some("stream_event" | "assistant" | "user" | "result")
            ) || event["type"] == "system" && event["subtype"] == "init")
        {
            bail!("claude_command_incomplete");
        }
        if event["type"] == "result" && event["origin"]["kind"] == "task-notification" {
            if !stamped {
                bail!("claude_unowned_task_result");
            }
            if let Some(consumed) = event["origin"]["runId"]
                .as_str()
                .and_then(|run| self.task_runs.get_mut(run))
            {
                *consumed = true;
            }
        }
        if event["type"] == "system"
            && event["subtype"] == "task_notification"
            && matches!(
                event["status"].as_str(),
                Some("completed" | "failed" | "stopped")
            )
        {
            if let (Some(task), Some(run)) = (
                Self::model_name(&event["task_id"]),
                Self::model_name(&event["run_id"]),
            ) {
                if !task.is_empty() && !self.task_runs.contains_key(run) {
                    if self.task_runs.len() >= 1024 {
                        bail!("claude_task_run_limit");
                    }
                    self.task_runs.insert(run.into(), false);
                }
            }
        }
        let mut out = vec![];
        if self.goals_supported {
            if let Some(turn) = &self.active {
                if let Some(mut ack) = crate::claude_goal::acknowledgement(&event, &self.user) {
                    ack["threadId"] = json!(self.session);
                    ack["turnId"] = json!(turn);
                    out.push(json!({"method":"thread/claude-goal/ack","params":ack}));
                }
            }
            for mut completion in crate::claude_goal::completions(&event) {
                completion["threadId"] = json!(self.session);
                out.push(json!({"method":"thread/claude-goal/completion","params":completion}));
            }
        }
        if event["type"] == "assistant"
            && parent.is_none()
            && self.input_settings_epoch == Some(self.settings_epoch)
            && self.model_revision <= self.input_model_revision
        {
            if let Some(model) =
                Self::model_name(&event["message"]["model"]).filter(|m| *m != "<synthetic>")
            {
                out.push(self.observe_model(model));
            }
        }
        if self.active.is_some() && !self.announced {
            self.announced = true;
            out.push(json!({"method":"turn/started","params":{"threadId":self.session,"turn":self.dispatched()["turn"]}}));
        }
        for item in folded {
            out.push(self.item_event(item));
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
                if let Some((turn, epoch)) = self.pending_interrupts.remove(&id) {
                    if r["subtype"] == "success" && epoch != self.invocation_epoch {
                        return Ok(vec![
                            json!({"id":id,"error":{"code":"claude_interrupt_outcome_unknown"}}),
                        ]);
                    }
                    if r["subtype"] == "success" && self.active.as_ref() == Some(&turn) {
                        self.interrupted = true;
                    }
                }
                let control = self.model_controls.remove(&id);
                let effort = self.effort_controls.remove(&id);
                let mut result = r["response"].clone();
                if r["subtype"] == "success" {
                    if let Some(candidate) = effort {
                        result = candidate;
                    }
                    if let Some(model) = control {
                        if model.is_empty() {
                            self.catalog(&result)?;
                        } else {
                            result = json!({"model":self.resolved(&model)});
                        }
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
                    if !e["index"].is_u64() {
                        bail!("claude_block_identity");
                    }
                    if self.message.is_empty() {
                        // Its message_start was not seen (scope opened mid-
                        // message): give the blocks a local message identity.
                        self.message = format!("message-{}", uuid::Uuid::new_v4());
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
                // A sub-agent's calls belong under its parent call while that
                // call runs (a foreground Agent). A background agent's parent
                // call already returned; its activity is shown by its task.
                if let Some(parent) = &parent {
                    if !self
                        .items
                        .get(parent)
                        .is_some_and(|item| item["status"] == "inProgress")
                    {
                        return Ok(out);
                    }
                }
                if let Some(blocks) = event["message"]["content"].as_array() {
                    for (index, b) in blocks.iter().enumerate() {
                        if b["type"] == "tool_result" {
                            let id = b["tool_use_id"]
                                .as_str()
                                .filter(|s| !s.is_empty() && s.len() <= 1024)
                                .ok_or_else(|| anyhow::anyhow!("claude_tool_id"))?
                                .to_owned();
                            let known = self.items.get(&id).cloned();
                            if known.is_none() && (parent.is_some() || event["isReplay"] == true) {
                                continue;
                            }
                            // A result whose call this scope never saw (call
                            // refused or evicted, result after a restart or a
                            // scope change) is still shown, as an untracked
                            // tool result, never dropped.
                            let mut item = known.unwrap_or_else(|| {
                                json!({"id":id,"type":"claudeToolCall","tool":"Tool result","input":null,"status":"inProgress","untracked":true})
                            });
                            item["status"] = json!(if b["is_error"] == true {
                                "failed"
                            } else {
                                "completed"
                            });
                            let text = tool_result_text(&b["content"]);
                            if item["type"] == "commandExecution" {
                                item["aggregatedOutput"] = json!(text);
                            } else {
                                item["output"] = json!(text);
                            }
                            if self.active.is_none() {
                                // No open turn: attach to the turn just closed.
                                if let Some(turn) = &self.last_turn {
                                    out.push(json!({"method":"item/completed","params":{"threadId":self.session,"turnId":turn,"item":item}}));
                                }
                                continue;
                            }
                            let item = self.retain(id, item)?;
                            out.push(self.item_event(item));
                        } else if !user {
                            if self.active.is_none() {
                                // Unowned synthetic/meta output: nothing to attach.
                                continue;
                            }
                            let message = event["message"]["id"]
                                .as_str()
                                .filter(|s| !s.is_empty() && s.len() <= 512)
                                .ok_or_else(|| anyhow::anyhow!("claude_message_id"))?;
                            // Sub-agent text and thinking are not the user's
                            // conversation; keep only its tool calls.
                            if parent.is_some() && b["type"] != "tool_use" {
                                continue;
                            }
                            // A final-only message can repeat another message's
                            // body. Only this same message owns streamed blocks.
                            let streamed = (parent.is_none() && message == self.message)
                                .then(|| {
                                    self.blocks
                                        .values()
                                        .find(|(_, old, _)| old == b)
                                        .map(|(id, _, _)| id.clone())
                                })
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
                self.last_turn = Some(turn.clone());
                self.input_commands.clear();
                self.active_input = None;
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
                self.sizes.clear();
                self.order.clear();
                self.blocks.clear();
            }
            // system idle may precede result: never dispatch another turn here.
            Some("system") => {
                if matches!(
                    event["subtype"].as_str(),
                    Some(
                        "background_tasks_changed"
                            | "task_started"
                            | "task_updated"
                            | "task_notification"
                    )
                ) {
                    out.push(json!({"method":"thread/tasks/updated","params":{"threadId":self.session,"event":event}}));
                }
                if event["subtype"] == "init"
                    && ((self.input_settings_epoch.is_none() && self.settings_epoch == 0)
                        || (self.input_settings_epoch == Some(self.settings_epoch)
                            && self.model_revision <= self.input_model_revision))
                {
                    if let Some(model) = Self::model_name(&event["model"]) {
                        out.push(self.observe_model(model));
                    }
                }
            }
            // Agents run without permission prompts; allow whatever Claude asks.
            Some("control_request") => {
                let request = &event["request"];
                let response = if request["subtype"] == "can_use_tool" {
                    json!({"behavior":"allow","updatedInput":request["input"]})
                } else {
                    json!({})
                };
                out.push(json!({"method":"claude/control-reply","params":{"wire":{"type":"control_response","response":{"subtype":"success","request_id":event["request_id"],"response":response}}}}));
            }
            _ => {}
        }
        Ok(out)
    }
}

#[cfg(test)]
mod task_followup_tests {
    use super::*;
    // Claude 2.1.312 shape: a task-notification invocation absorbs the input
    // queued mid-turn and stamps its later replies and result with that uuid.
    fn absorbed_input_turn(notification_after_init: bool) {
        let mut a = Adapter::new("session".into()).unwrap();
        let notification = json!({"type":"system","subtype":"task_notification","session_id":"session","task_id":"task","run_id":"run","status":"completed"});
        if !notification_after_init {
            a.consume(notification.clone()).unwrap();
        }
        a.consume(json!({"type":"system","subtype":"init","session_id":"session"}))
            .unwrap();
        if notification_after_init {
            a.consume(notification).unwrap();
        }
        a.consume(json!({"type":"stream_event","session_id":"session","event":{"type":"message_start","message":{"id":"m1"}}})).unwrap();
        a.consume(json!({"type":"stream_event","session_id":"session","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"t1","name":"Bash","input":{}}}})).unwrap();
        a.consume(json!({"type":"assistant","session_id":"session","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}]}})).unwrap();
        let Request::Input { wire, dispatched } = a
            .request(
                1,
                "turn/start",
                &json!({"input":[{"type":"text","text":"B"}]}),
            )
            .unwrap()
        else {
            panic!()
        };
        let input = wire["uuid"].clone();
        let turn = dispatched["turn"]["id"].clone();
        for state in ["queued", "started"] {
            a.consume(json!({"type":"command_lifecycle","session_id":"session","command_uuid":input,"state":state})).unwrap();
        }
        a.consume(json!({"type":"user","session_id":"session","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}})).unwrap();
        if notification_after_init {
            a.consume(json!({"type":"system","subtype":"init","session_id":"session"}))
                .unwrap();
        }
        let started = a.consume(json!({"type":"stream_event","session_id":"session","user_message_uuid":input,"user_message_uuids":[input],"event":{"type":"message_start","message":{"id":"m2"}}})).unwrap();
        assert_eq!(started[0]["method"], "turn/started");
        assert_eq!(started[0]["params"]["turn"]["id"], turn);
        a.consume(json!({"type":"assistant","session_id":"session","user_message_uuid":input,"user_message_uuids":[input],"message":{"id":"m2","content":[{"type":"text","text":"answer B"}]}})).unwrap();
        a.consume(json!({"type":"command_lifecycle","session_id":"session","command_uuid":input,"state":"completed"})).unwrap();
        let done = a.consume(json!({"type":"result","session_id":"session","result":"answer B","origin":{"kind":"task-notification","producer":"session-task","runId":"run"},"user_message_uuid":input,"user_message_uuids":[input]})).unwrap();
        let completed = &done.last().unwrap()["params"]["turn"];
        assert_eq!(done.last().unwrap()["method"], "turn/completed");
        assert_eq!(completed["id"], turn);
        assert_eq!(completed["status"], "completed");
        let items = completed["items"].as_array().unwrap();
        assert!(items
            .iter()
            .any(|i| i["id"] == "t1" && i["status"] == "completed"));
        assert!(items.iter().any(|i| i["text"] == "answer B"));
        assert!(a.idle_for_disposition());
        assert_eq!(a.task_runs.get("run"), Some(&true));
    }
    #[test]
    fn task_invocation_absorbing_queued_input_completes_that_input_turn() {
        absorbed_input_turn(false);
    }
    #[test]
    fn late_task_notification_and_repeated_init_still_complete_absorbed_turn() {
        absorbed_input_turn(true);
    }
    #[test]
    fn scheduled_invocation_epoch_protects_interrupt_and_waiting_input() {
        let mut a = Adapter::new("session".into()).unwrap();
        let Request::Input { wire, .. } = a
            .request(
                1,
                "turn/start",
                &json!({"input":[{"type":"text","text":"B"}]}),
            )
            .unwrap()
        else {
            panic!()
        };
        let input = wire["uuid"].clone();
        a.request(2, "turn/interrupt", &json!({})).unwrap();
        let id = "91d7ab21-8b8e-40ce-913b-4eb60e7e6fa2";
        let start = json!({"type":"command_lifecycle","session_id":"session","command_uuid":id,"state":"started"});
        a.consume(start.clone()).unwrap();
        assert!(!a.idle_for_disposition());
        assert!(a.request(3, "turn/interrupt", &json!({})).is_err());
        assert!(a.request(4,"turn/steer",&json!({"threadId":"session","expectedTurnId":a.active,"input":[{"type":"text","text":"not Cron input"}]})).is_err());
        a.consume(json!({"type":"system","subtype":"init","session_id":"session"}))
            .unwrap();
        a.consume(json!({"type":"result","session_id":"session","result":"cron"}))
            .unwrap();
        assert!(!a.idle_for_disposition());
        let output=a.consume(json!({"type":"command_lifecycle","session_id":"session","command_uuid":id,"state":"completed"})).unwrap();
        assert_eq!(output[0]["method"], "thread/claude-scheduled-output");
        let ack=a.consume(json!({"type":"control_response","response":{"subtype":"success","request_id":"2","response":{}}})).unwrap();
        assert_eq!(ack[0]["error"]["code"], "claude_interrupt_outcome_unknown");
        assert!(!a.interrupted);
        // The local input's started/init/result remain ordinary, not Cron.
        a.consume(json!({"type":"command_lifecycle","session_id":"session","command_uuid":input,"state":"started"})).unwrap();
        a.consume(json!({"type":"system","subtype":"init","session_id":"session","user_message_uuids":[input]})).unwrap();
        let result = a
            .consume(json!({"type":"result","session_id":"session","user_message_uuids":[input]}))
            .unwrap();
        assert_eq!(
            result.last().unwrap()["params"]["turn"]["status"],
            "completed"
        );
        assert!(
            a.consume(start).is_err(),
            "duplicate command must not arm another invocation"
        );
    }
    #[test]
    fn scheduled_scope_requires_exact_session_init_and_completion() {
        for fault in ["session", "init", "completion", "origin", "input"] {
            let mut a = Adapter::new("session".into()).unwrap();
            let start = json!({"type":"command_lifecycle","session_id":if fault=="session"{"other"}else{"session"},"command_uuid":"91d7ab21-8b8e-40ce-913b-4eb60e7e6fa2","state":"started"});
            if fault == "session" {
                assert!(a.consume(start).is_err());
                continue;
            }
            a.consume(start).unwrap();
            if fault == "init" {
                assert!(a.consume(json!({"type":"stream_event"})).is_err());
                continue;
            }
            a.consume(json!({"type":"system","subtype":"init","session_id":"session"}))
                .unwrap();
            if fault == "completion" {
                assert!(a.consume(json!({"type":"command_lifecycle","session_id":"session","command_uuid":"91d7ab21-8b8e-40ce-913b-4eb60e7e6fa2","state":"completed"})).is_err());
                continue;
            }
            let mut result = json!({"type":"result","session_id":"session"});
            if fault == "origin" {
                result["origin"] = json!({"kind":"unknown"});
            } else {
                result["user_message_uuid"] = json!("waiting-input");
            }
            assert!(a.consume(result).is_err());
        }
    }
    #[test]
    fn scheduled_start_during_steer_write_is_unknown_without_retaining_input() {
        let mut a = Adapter::new("session".into()).unwrap();
        a.request(
            1,
            "turn/start",
            &json!({"input":[{"type":"text","text":"B"}]}),
        )
        .unwrap();
        let Request::Steer {turn,epoch,item,after,..}=a.request(2,"turn/steer",&json!({"threadId":"session","expectedTurnId":a.active,"input":[{"type":"text","text":"not proven"}]})).unwrap() else {panic!()};
        a.consume(json!({"type":"command_lifecycle","session_id":"session","command_uuid":"91d7ab21-8b8e-40ce-913b-4eb60e7e6fa2","state":"started"})).unwrap();
        assert_eq!(
            a.steered(&turn, epoch, item, after.as_deref())
                .unwrap_err()
                .to_string(),
            "claude_steer_outcome_unknown"
        );
        assert_eq!(a.dispatched()["turn"]["items"].as_array().unwrap().len(), 1);
    }
    #[test]
    fn refusal_diagnostics_never_publish_error_context_or_native_content() {
        let a = Adapter::new("private-session".into()).unwrap();
        let refusal = a.refusal_diagnostic(&anyhow::anyhow!("claude_unowned_stream"));
        assert_eq!(refusal["code"], "claude_unowned_stream");
        for error in [
            "secret provider body",
            "claude_unowned_stream: private context",
            "private-session",
        ] {
            let diagnostic = a.refusal_diagnostic(&anyhow::anyhow!(error.to_owned()));
            assert_eq!(diagnostic["code"], "claude_protocol_refused");
            assert!(!diagnostic.to_string().contains(error));
        }
    }
    #[test]
    fn interrupt_cannot_borrow_task_invocation_ack_for_waiting_input() {
        let mut a = Adapter::new("session".into()).unwrap();
        a.active = Some("waiting-B".into());
        assert!(matches!(
            a.request(10, "turn/interrupt", &json!({})).unwrap(),
            Request::Wire(_)
        ));
        a.consume(json!({"type":"system","subtype":"task_notification","session_id":"session","task_id":"task","run_id":"run","status":"completed"})).unwrap();
        a.consume(json!({"type":"system","subtype":"init","session_id":"session"}))
            .unwrap();
        // Refusal is BEFORE producing any native control/write request.
        assert!(a.request(11, "turn/interrupt", &json!({})).is_err());
        // Even if the task completes before the interrupt ACK, the epoch must
        // remember that it crossed an invocation boundary.
        a.consume(json!({"type":"result","session_id":"session","origin":{"kind":"task-notification","producer":"session-task","runId":"run"}})).unwrap();
        let ack=a.consume(json!({"type":"control_response","response":{"request_id":"10","subtype":"success","response":{}}})).unwrap();
        assert_eq!(ack[0]["error"]["code"], "claude_interrupt_outcome_unknown");
        assert!(!a.interrupted);
        assert!(a.pending_interrupts.is_empty());
        let result = a
            .consume(
                json!({"type":"result","session_id":"session","result":"B actually completed"}),
            )
            .unwrap();
        assert_eq!(
            result.last().unwrap()["params"]["turn"]["status"],
            "completed"
        );
    }
    #[test]
    fn unowned_task_result_cannot_complete_current_input() {
        let mut a = Adapter::new("session".into()).unwrap();
        a.active = Some("local-input".into());
        assert!(a.consume(json!({"type":"result","session_id":"session","origin":{"kind":"task-notification","producer":"session-task","runId":"unknown"}})).is_err());
        assert_eq!(a.active.as_deref(), Some("local-input"));
    }
    #[test]
    fn buffered_task_is_not_idle_and_duplicate_run_cannot_arm_it_again() {
        let mut a = Adapter::new("session".into()).unwrap();
        let notification = json!({"type":"system","subtype":"task_notification","session_id":"session","task_id":"own-task","run_id":"run","status":"completed"});
        a.consume(notification.clone()).unwrap();
        a.consume(json!({"type":"system","subtype":"init","session_id":"session"}))
            .unwrap();
        assert!(!a.idle_for_disposition());
        let output=a.consume(json!({"type":"result","session_id":"session","result":"task result","origin":{"kind":"task-notification","producer":"session-task","runId":"run"}})).unwrap();
        assert_eq!(output[0]["method"], "thread/claude-task-output");
        assert!(a.idle_for_disposition());
        a.consume(notification).unwrap();
        a.consume(json!({"type":"system","subtype":"init","session_id":"session"}))
            .unwrap();
        assert!(a.consume(json!({"type":"stream_event","event":{"type":"message_start","message":{"id":"duplicate"}}})).is_err());
    }
}
#[cfg(test)]
mod degrade_tests {
    use super::*;
    const S: &str = "session";
    fn ev(mut v: Value) -> Value {
        v["session_id"] = json!(S);
        v
    }
    fn input(a: &mut Adapter, id: u64, text: &str) -> Value {
        let Request::Input { wire, .. } = a
            .request(
                id,
                "turn/start",
                &json!({"input":[{"type":"text","text":text}]}),
            )
            .unwrap()
        else {
            panic!()
        };
        wire["uuid"].clone()
    }
    fn task_output(out: &[Value]) -> Value {
        out.iter()
            .find(|m| m["method"] == "thread/claude-task-output")
            .expect("task output")["params"]["turn"]
            .clone()
    }
    // Live shape (Claude 2.1.314): background Agent calls return at once;
    // the sub-agents' tool calls and results arrive later with
    // parent_tool_use_id set, while idle or inside a task invocation.
    #[test]
    fn background_subagent_frames_never_refuse_or_clobber_task_output() {
        let mut a = Adapter::new(S.into()).unwrap();
        let uuid = input(&mut a, 1, "launch agents");
        a.consume(ev(
            json!({"type":"system","subtype":"init","user_message_uuids":[uuid]}),
        ))
        .unwrap();
        a.consume(ev(json!({"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"agent-1","name":"Agent","input":{"run_in_background":true}}]}}))).unwrap();
        a.consume(ev(json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"agent-1","content":"launched"}]}}))).unwrap();
        a.consume(ev(
            json!({"type":"result","result":"started","user_message_uuids":[uuid]}),
        ))
        .unwrap();
        // Idle: the sub-agent works.
        let sub_call = ev(
            json!({"type":"assistant","parent_tool_use_id":"agent-1","message":{"id":"sub-m","model":"sub-model","content":[{"type":"tool_use","id":"sub-bash","name":"Bash","input":{"command":"ls"}}]}}),
        );
        let sub_result = ev(
            json!({"type":"user","parent_tool_use_id":"agent-1","message":{"content":[{"type":"tool_result","tool_use_id":"sub-bash","content":"files"}]}}),
        );
        assert!(a.consume(sub_call.clone()).unwrap().is_empty());
        assert!(a.consume(sub_result.clone()).unwrap().is_empty());
        assert!(a.consume(ev(json!({"type":"stream_event","parent_tool_use_id":"agent-1","event":{"type":"message_start","message":{"id":"sub-m2"}}}))).unwrap().is_empty());
        assert!(
            a.idle_for_disposition(),
            "sub-agent frames open no invocation"
        );
        // Its completion starts a task invocation; sub-agent frames of other
        // agents interleave with that invocation's stream.
        a.consume(ev(json!({"type":"system","subtype":"task_notification","task_id":"t","run_id":"run","status":"completed"}))).unwrap();
        a.consume(ev(json!({"type":"system","subtype":"init"})))
            .unwrap();
        a.consume(ev(
            json!({"type":"stream_event","event":{"type":"message_start","message":{"id":"m2"}}}),
        ))
        .unwrap();
        a.consume(ev(json!({"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}}))).unwrap();
        a.consume(ev(json!({"type":"stream_event","parent_tool_use_id":"agent-2","event":{"type":"message_start","message":{"id":"other"}}}))).unwrap();
        a.consume(sub_call).unwrap();
        a.consume(sub_result).unwrap();
        a.consume(ev(json!({"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"agent done"}}}))).unwrap();
        let out = a.consume(ev(json!({"type":"result","result":"agent done","origin":{"kind":"task-notification","producer":"session-task","runId":"run"}}))).unwrap();
        let turn = task_output(&out);
        let items = turn["items"].as_array().unwrap();
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(items[0]["text"], "agent done");
        assert!(a.idle_for_disposition());
        // A sub-agent's model never becomes the session's model.
        assert_ne!(a.current_model.as_deref(), Some("sub-model"));
    }
    #[test]
    fn foreground_subagent_calls_stay_under_the_running_parent_call() {
        let mut a = Adapter::new(S.into()).unwrap();
        input(&mut a, 1, "go");
        a.consume(ev(json!({"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"agent","name":"Agent","input":{}}]}}))).unwrap();
        let out = a.consume(ev(json!({"type":"assistant","parent_tool_use_id":"agent","message":{"id":"s","content":[{"type":"text","text":"sub text"},{"type":"tool_use","id":"sub","name":"Read","input":{}}]}}))).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["params"]["item"]["id"], "sub");
        let out = a.consume(ev(json!({"type":"user","parent_tool_use_id":"agent","message":{"content":[{"type":"tool_result","tool_use_id":"sub","content":"read"}]}}))).unwrap();
        assert_eq!(out[0]["params"]["item"]["status"], "completed");
    }
    #[test]
    fn unknown_result_inside_task_invocation_is_shown_and_keeps_the_invocation() {
        let mut a = Adapter::new(S.into()).unwrap();
        a.consume(ev(json!({"type":"system","subtype":"task_notification","task_id":"t","run_id":"run","status":"completed"}))).unwrap();
        a.consume(ev(json!({"type":"system","subtype":"init"})))
            .unwrap();
        a.consume(ev(json!({"type":"assistant","message":{"id":"m","content":[{"type":"text","text":"before"}]}}))).unwrap();
        a.consume(ev(json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"never-seen","content":[{"type":"text","text":"out"},{"type":"image","source":{"data":"AAAA"}}]}]}}))).unwrap();
        let out = a.consume(ev(json!({"type":"result","origin":{"kind":"task-notification","producer":"session-task","runId":"run"}}))).unwrap();
        let items = task_output(&out)["items"].as_array().unwrap().clone();
        assert_eq!(items[0]["text"], "before");
        assert_eq!(items[1]["output"], "out\n[image]");
    }
    #[test]
    fn result_after_its_turn_closed_attaches_to_that_turn() {
        let mut a = Adapter::new(S.into()).unwrap();
        input(&mut a, 1, "go");
        let done = a
            .consume(ev(json!({"type":"result","result":"ok"})))
            .unwrap();
        let turn = done.last().unwrap()["params"]["turn"]["id"].clone();
        let late = a.consume(ev(json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"bg","content":"late"}]}}))).unwrap();
        assert_eq!(late[0]["method"], "item/completed");
        assert_eq!(late[0]["params"]["turnId"], turn);
        assert_eq!(late[0]["params"]["item"]["output"], "late");
    }
    #[test]
    fn untracked_invocation_output_is_displayed_not_dropped() {
        let mut a = Adapter::new(S.into()).unwrap();
        // No announced task run (or output after a restart): still shown.
        a.consume(ev(
            json!({"type":"stream_event","event":{"type":"message_start","message":{"id":"m"}}}),
        ))
        .unwrap();
        a.consume(ev(json!({"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":"hello"}}}))).unwrap();
        assert!(!a.idle_for_disposition());
        let out = a.consume(ev(json!({"type":"result","result":"hello","origin":{"kind":"task-notification","producer":"session-task","runId":"unknown"}}))).unwrap();
        let turn = task_output(&out);
        assert_eq!(turn["items"][0]["text"], "hello");
        assert_eq!(turn["projectionOnly"], true);
        assert!(a.idle_for_disposition());
        assert!(a.task_runs.is_empty(), "an untracked run is not armed");
        // Then an ordinary input still works.
        let uuid = input(&mut a, 2, "next");
        let out = a
            .consume(ev(
                json!({"type":"result","result":"fine","user_message_uuids":[uuid]}),
            ))
            .unwrap();
        assert_eq!(out.last().unwrap()["method"], "turn/completed");
    }
    #[test]
    fn block_without_message_start_gets_a_local_message_identity() {
        let mut a = Adapter::new(S.into()).unwrap();
        input(&mut a, 1, "go");
        let out = a.consume(ev(json!({"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":"mid"}}}))).unwrap();
        assert_eq!(out.last().unwrap()["params"]["item"]["text"], "mid");
    }
    #[test]
    fn new_input_invocation_salvages_an_unfinished_task_invocation() {
        for absorbed in [true, false] {
            let mut a = Adapter::new(S.into()).unwrap();
            a.consume(ev(json!({"type":"system","subtype":"task_notification","task_id":"t","run_id":"run","status":"completed"}))).unwrap();
            a.consume(ev(json!({"type":"system","subtype":"init"})))
                .unwrap();
            a.consume(ev(json!({"type":"assistant","message":{"id":"m","content":[{"type":"text","text":"partial task"}]}}))).unwrap();
            let uuid = input(&mut a, 1, "next");
            // Stamped with the waiting input: the task output is folded into
            // its turn. Any other input invocation: shown as task output.
            let stamp = if absorbed {
                uuid.clone()
            } else {
                json!("other-client")
            };
            let out = a
                .consume(ev(
                    json!({"type":"system","subtype":"init","user_message_uuids":[stamp]}),
                ))
                .unwrap();
            if !absorbed {
                assert_eq!(task_output(&out)["items"][0]["text"], "partial task");
            }
            a.consume(ev(json!({"type":"assistant","message":{"id":"m2","content":[{"type":"text","text":"answer"}]}}))).unwrap();
            let out = a
                .consume(ev(json!({"type":"result","user_message_uuids":[uuid]})))
                .unwrap();
            let turn = &out.last().unwrap()["params"]["turn"];
            assert_eq!(turn["status"], "completed");
            let items = turn["items"].as_array().unwrap();
            assert!(items.iter().any(|i| i["text"] == "answer"));
            assert_eq!(items.iter().any(|i| i["text"] == "partial task"), absorbed);
        }
    }
    #[test]
    fn refused_frame_salvages_buffered_task_output() {
        let mut a = Adapter::new(S.into()).unwrap();
        a.consume(ev(json!({"type":"system","subtype":"task_notification","task_id":"t","run_id":"run","status":"completed"}))).unwrap();
        a.consume(ev(json!({"type":"system","subtype":"init"})))
            .unwrap();
        a.consume(ev(json!({"type":"assistant","message":{"id":"m","content":[{"type":"text","text":"kept"}]}}))).unwrap();
        assert!(a
            .consume(ev(json!({"type":"result","origin":{"kind":"other"}})))
            .is_err());
        let out = a.skip_refused_frame();
        assert_eq!(task_output(&out)["items"][0]["text"], "kept");
        assert!(a.idle_for_disposition());
    }
    #[test]
    fn huge_tool_output_is_capped_and_turn_budget_degrades_instead_of_refusing() {
        let mut a = Adapter::new(S.into()).unwrap();
        input(&mut a, 1, "go");
        let big = "x".repeat(DISPLAY_OUTPUT_BYTES * 8);
        for n in 0..200 {
            let id = format!("read-{n}");
            a.consume(ev(json!({"type":"assistant","message":{"id":format!("m{n}"),"content":[{"type":"tool_use","id":id,"name":"Read","input":{"file_path":"f"}}]}}))).unwrap();
            let out = a.consume(ev(json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":id,"content":big}]}}))).unwrap();
            let output = out.last().unwrap()["params"]["item"]["output"]
                .as_str()
                .unwrap()
                .len();
            assert!(output <= DISPLAY_OUTPUT_BYTES + 256, "{output}");
        }
        let total: usize = a.items.values().map(|v| v.to_string().len()).sum();
        assert!(total <= TURN_BYTES);
        assert_eq!(a.sizes.values().sum::<usize>(), total, "cached sizes match");
        let out = a
            .consume(ev(json!({"type":"result","result":"done"})))
            .unwrap();
        assert_eq!(
            out.last().unwrap()["params"]["turn"]["items"]
                .as_array()
                .unwrap()
                .len(),
            202
        );
    }
    #[test]
    fn display_text_keeps_utf8_head_and_tail() {
        let text = "é".repeat(50_000);
        let capped = display_text(&text, 1000);
        assert!(capped.len() < 1200);
        assert!(capped.starts_with('é') && capped.ends_with('é'));
        assert_eq!(display_text("short", 1000), "short");
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
        let mut a = Adapter::new("session".into()).unwrap();
        a.request(1, "initialize", &json!({})).unwrap();
        ack(
            &mut a,
            1,
            json!({"models":[{"value":"default","resolvedModel":"sonnet-version"},{"value":"sonnet","resolvedModel":"sonnet-version"},{"value":"opus","resolvedModel":"opus-version"}]}),
        );
        assert_eq!(a.models.len(), 2);
        assert_eq!(a.models[0]["id"], "sonnet");
        a.request(
            2,
            "turn/start",
            &json!({"input":[{"type":"text","text":"A"}]}),
        )
        .unwrap();
        a.request(3, "model/set", &json!({"model":"opus"})).unwrap();
        let result = ack(&mut a, 3, json!({}));
        assert_eq!(result.last().unwrap()["result"]["model"], "opus-version");
        a.commit_model(&result.last().unwrap()["result"]).unwrap();
        let late=a.consume(json!({"type":"assistant","message":{"id":"old-A","model":"sonnet-version","content":[{"type":"text","text":"old model output"}]}})).unwrap();
        assert!(!late.iter().any(|e| e["method"] == "thread/model/updated"));
        assert_eq!(a.current_model.as_deref(), Some("opus-version"));
        a.request(4, "model/set", &json!({"model":"expired"}))
            .unwrap();
        a.forget_control(4);
        ack(&mut a, 4, json!({}));
        assert_eq!(a.current_model.as_deref(), Some("opus-version"));
        // An ACK parsed in the drop/forget gap still cannot commit a choice:
        // the timed-out RPC future never consumes this candidate result.
        a.request(6, "model/set", &json!({"model":"gap"})).unwrap();
        let candidate = ack(&mut a, 6, json!({}));
        assert_eq!(candidate.last().unwrap()["result"]["model"], "gap");
        a.forget_control(6);
        assert_eq!(a.current_model.as_deref(), Some("opus-version"));
        a.consume(json!({"type":"result","subtype":"success","is_error":false}))
            .unwrap();
        let stale = a
            .consume(json!({"type":"system","subtype":"init","model":"old-after-timeout"}))
            .unwrap();
        assert!(!stale.iter().any(|e| e["method"] == "thread/model/updated"));
        assert_eq!(a.current_model.as_deref(), Some("opus-version"));
        a.request(
            5,
            "turn/start",
            &json!({"input":[{"type":"text","text":"B"}]}),
        )
        .unwrap();
        let fresh = a
            .consume(json!({"type":"system","subtype":"init","model":"reported-actual"}))
            .unwrap();
        assert!(fresh
            .iter()
            .any(|e| e["params"]["model"] == "reported-actual"));
        assert_eq!(a.current_model.as_deref(), Some("reported-actual"));
    }
    #[test]
    fn malformed_catalog_refuses_boundedly_and_invalid_model_never_emits_control() {
        let mut a = Adapter::new("session".into()).unwrap();
        assert!(a
            .request(2, "model/set", &json!({"model":"bad\nmodel"}))
            .is_err());
        a.request(1, "initialize", &json!({})).unwrap();
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
        if bytes.len() + end > IMAGE_BASE64_BYTES + 2 * 1024 * 1024 {
            bail!("claude_frame_limit");
        }
        bytes.extend_from_slice(&chunk[..end]);
        reader.consume(end);
        if bytes.last() == Some(&b'\n') {
            let mut frame = crate::bounded_json::decode(&bytes)
                .map_err(|_| anyhow::anyhow!("claude_invalid_frame"))?;
            if bytes.len() > 2 * 1024 * 1024 {
                // Claude can echo the submitted image blocks. Validate then
                // discard only those payloads; user echo never settles input.
                // Nonimage data retains the original two-MiB admission cap.
                if frame["type"] != "user" {
                    bail!("claude_frame_limit");
                }
                let content = frame["message"]["content"]
                    .as_array_mut()
                    .ok_or_else(|| anyhow::anyhow!("claude_frame_limit"))?;
                let mut count = 0;
                let mut total = 0;
                for block in content {
                    if block["type"] == "image" {
                        count += 1;
                        total += image_source(&block["source"])?;
                        if count > crate::image_format::COUNT_LIMIT || total > IMAGE_BYTES {
                            bail!("claude_frame_limit");
                        }
                        block["source"]["data"] = json!("");
                    }
                }
                if count == 0 || serde_json::to_vec(&frame)?.len() > 2 * 1024 * 1024 {
                    bail!("claude_frame_limit");
                }
            }
            return Ok(Some(frame));
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
    fn image_inputs_require_bounded_valid_native_image_bytes() {
        use base64::Engine;
        let data = "R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==";
        let source = json!({"type":"base64","media_type":"image/gif","data":data});
        assert!(image_source(&source).is_ok());
        for invalid in [
            json!({"type":"url","media_type":"image/gif","data":data}),
            json!({"type":"base64","media_type":"image/png","data":data}),
            json!({"type":"base64","media_type":"image/gif","data":"!!!!"}),
            json!({"type":"base64","media_type":"image/gif","data":base64::engine::general_purpose::STANDARD.encode(b"not an image")}),
        ] {
            assert!(image_source(&invalid).is_err());
        }
        let mut a = adapter();
        let blocks = vec![json!({"type":"image","source":source}); 9];
        assert!(a
            .request(1, "turn/start", &json!({"input":blocks}))
            .is_err());
        assert!(
            a.active.is_none(),
            "invalid images refuse before starting input"
        );
        assert!(a
            .request(
                2,
                "turn/start",
                &json!({"input":[{"type":"localImage","path":"/not/admitted"}]})
            )
            .is_err());
    }
    #[tokio::test]
    async fn large_user_image_echo_is_bounded_validated_and_never_becomes_turn_authority() {
        use base64::Engine;
        let mut image = base64::engine::general_purpose::STANDARD
            .decode("R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==")
            .unwrap();
        image.pop();
        image.extend([0x21, 0xfe]);
        for _ in 0..8192 {
            image.push(255);
            image.extend([65; 255]);
        }
        image.extend([0, 0x3b]);
        let source = json!({"type":"base64","media_type":"image/gif","data":base64::engine::general_purpose::STANDARD.encode(image)});
        let frame = json!({"type":"user","message":{"content":[{"type":"image","source":source}]}});
        let mut bytes = serde_json::to_vec(&frame).unwrap();
        assert!(bytes.len() > 2 * 1024 * 1024);
        bytes.extend(b"\n{\"type\":\"result\",\"result\":\"done\"}\n");
        let mut reader = tokio::io::BufReader::new(bytes.as_slice());
        let echo = line(&mut reader).await.unwrap().unwrap();
        assert_eq!(echo["message"]["content"][0]["source"]["data"], "");
        let mut a = adapter();
        start(&mut a);
        let events = a.consume(echo).unwrap();
        assert!(!events.iter().any(|e| e["method"] == "turn/completed"));
        assert!(a.active.is_some());
        assert_eq!(
            a.dispatched()["turn"]["items"].as_array().unwrap().len(),
            1,
            "echo adds no input receipt or item"
        );
        let result = line(&mut reader).await.unwrap().unwrap();
        assert_eq!(result["type"], "result");
        let oversized = json!({"type":"assistant","message":{"content":"x".repeat(2*1024*1024)}})
            .to_string()
            + "\n";
        assert!(line(&mut tokio::io::BufReader::new(oversized.as_bytes()))
            .await
            .is_err());
        let mut fake = frame;
        fake["message"]["content"][0]["source"]["media_type"] = json!("image/png");
        let malformed = fake.to_string() + "\n";
        assert!(line(&mut tokio::io::BufReader::new(malformed.as_bytes()))
            .await
            .is_err());
    }
    #[test]
    fn steering_is_exact_turn_input_not_interrupt_or_new_turn() {
        let mut a = adapter();
        start(&mut a);
        let turn = a.active.clone().unwrap();
        a.consume(json!({"type":"assistant","message":{"id":"before","content":[{"type":"text","text":"before steer"}]}})).unwrap();
        for id in [2, 3] {
            let request=a.request(id,"turn/steer",&json!({"threadId":"synthetic-session","expectedTurnId":turn,"input":[{"type":"text","text":"same steer"}]})).unwrap();
            let Request::Steer {
                wire,
                turn: expected,
                epoch,
                item,
                after,
            } = request
            else {
                panic!("expected native steer")
            };
            let dispatched = a.steered(&expected, epoch, item, after.as_deref()).unwrap();
            assert_eq!(wire["type"], "user");
            assert!(wire["uuid"].is_string());
            assert_eq!(wire["message"]["content"][0]["text"], "same steer");
            assert_eq!(dispatched["turn"]["id"], turn);
        }
        assert!(a
            .request(
                4,
                "turn/steer",
                &json!({"threadId":"wrong","expectedTurnId":turn,"input":[]})
            )
            .is_err());
        assert!(a
            .request(
                5,
                "turn/steer",
                &json!({"threadId":"synthetic-session","expectedTurnId":"wrong","input":[]})
            )
            .is_err());
        let final_events = a.consume(json!({"type":"result","result":"done"})).unwrap();
        let items = final_events.last().unwrap()["params"]["turn"]["items"]
            .as_array()
            .unwrap();
        assert_eq!(
            items
                .iter()
                .map(|i| i["text"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "local user",
                "before steer",
                "same steer",
                "same steer",
                "done"
            ]
        );
        assert_ne!(items[2]["id"], items[3]["id"]);
        assert!(a
            .request(
                6,
                "turn/steer",
                &json!({"threadId":"synthetic-session","expectedTurnId":turn,"input":[]})
            )
            .is_err());
    }
    #[test]
    fn result_during_steer_write_starts_the_next_turn_not_old_turn_completion() {
        let mut a = adapter();
        start(&mut a);
        let turn = a.active.clone().unwrap();
        let Request::Steer {turn:expected,epoch,item,after,..}=a.request(2,"turn/steer",&json!({"threadId":"synthetic-session","expectedTurnId":turn,"input":[{"type":"text","text":"raced steer"}]})).unwrap() else {panic!()};
        let completed = a
            .consume(json!({"type":"result","result":"old turn result"}))
            .unwrap();
        let items = completed.last().unwrap()["params"]["turn"]["items"]
            .as_array()
            .unwrap();
        assert!(!items.iter().any(|i| i["text"] == "raced steer"));
        // Claude runs the queued input next: it becomes its own turn.
        let next = a.steered(&expected, epoch, item, after.as_deref()).unwrap();
        assert_ne!(next["turn"]["id"], json!(turn));
        assert_eq!(next["turn"]["items"][0]["text"], "raced steer");
        let started = a
            .consume(json!({"type":"system","subtype":"init"}))
            .unwrap();
        assert_eq!(started[0]["method"], "turn/started");
        let done = a
            .consume(json!({"type":"result","result":"steer answered"}))
            .unwrap();
        assert_eq!(
            done.last().unwrap()["params"]["turn"]["id"],
            next["turn"]["id"]
        );
    }
    #[test]
    fn unknown_frames_are_ignored_and_permission_requests_are_allowed() {
        let mut a = adapter();
        start(&mut a);
        assert!(a
            .consume(json!({"type":"brand_new_frame_type","session_id":"synthetic-session"}))
            .is_ok());
        let out=a.consume(json!({"type":"control_request","request_id":"req-1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"ls"}}})).unwrap();
        let reply = &out.last().unwrap()["params"]["wire"];
        assert_eq!(reply["response"]["request_id"], "req-1");
        assert_eq!(reply["response"]["response"]["behavior"], "allow");
        assert_eq!(
            reply["response"]["response"]["updatedInput"]["command"],
            "ls"
        );
        assert!(
            a.consume(json!({"type":"result","session_id":"another-session"}))
                .is_err(),
            "only a session mismatch is refused"
        );
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
        // An unknown call's result is shown as an untracked item, not refused.
        let shown = a.consume(json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"unknown","content":"late output"}]}})).unwrap();
        assert_eq!(
            shown.last().unwrap()["params"]["item"]["output"],
            "late output"
        );
        assert_eq!(shown.last().unwrap()["params"]["item"]["untracked"], true);
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
        a.request(
            1,
            "turn/start",
            &json!({"input":[{"type":"text","text":"prompt"}]}),
        )
        .unwrap();
        a.consume(json!({"type":"stream_event","event":{"type":"message_start","message":{"id":"message-a"}}})).unwrap();
        a.consume(json!({"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":"same"}}})).unwrap();
        for id in ["message-a", "message-a", "message-b"] {
            a.consume(json!({"type":"assistant","message":{"id":id,"content":[{"type":"text","text":"same"}]}})).unwrap();
        }
        assert_eq!(a.order, vec!["message-a:0", "message-b:0"]);
        assert_eq!(a.items["message-a:0"]["text"], "same");
        assert_eq!(a.items["message-b:0"]["text"], "same");
    }
}
#[cfg(test)]
mod effort_tests {
    use super::*;
    fn ack(adapter: &mut Adapter, id: u64, response: Value) -> Vec<Value> {
        adapter.consume(json!({"type":"control_response","response":{"subtype":"success","request_id":id.to_string(),"response":response}})).unwrap()
    }
    #[test]
    fn effort_probe_and_ack_are_candidates_only_until_waiter_commits() {
        let mut a = Adapter::new("session".into()).unwrap();
        a.request(1, "initialize", &json!({})).unwrap();
        ack(
            &mut a,
            1,
            json!({"models":[{"value":"default","resolvedModel":"native","supportsEffort":true,"supportedEffortLevels":["high"," HIGH ","bad",17]}]}),
        );
        assert_eq!(
            a.visible_models()[0]["supportedReasoningEfforts"],
            json!([])
        );
        a.request(2, "effort/probe", &json!({})).unwrap();
        let result = ack(&mut a, 2, json!({}));
        assert!(!a.effort_supported);
        a.commit_effort(&result[0]["result"]).unwrap();
        assert_eq!(
            a.visible_models()[0]["supportedReasoningEfforts"],
            json!([{"reasoningEffort":"high","description":""}])
        );
        a.request(3, "effort/set", &json!({"reasoningEffort":"high"}))
            .unwrap();
        let result = ack(&mut a, 3, json!({}));
        a.forget_control(3);
        assert_eq!(result[0]["result"]["effort"], "high");
        assert!(!a.effort_known, "parse-before-cancel does not commit");
        a.request(4, "effort/set", &json!({"reasoningEffort":null}))
            .unwrap();
        a.forget_control(4);
        let late = ack(&mut a, 4, json!({}));
        assert!(a.commit_effort(&late[0]["result"]).is_err());
        assert!(!a.effort_known);
        a.request(5, "effort/set", &json!({"reasoningEffort":"high"}))
            .unwrap();
        let prior = a.observe_model("native");
        let response = ack(&mut a, 5, json!({}));
        let committed = a.commit_effort(&response[0]["result"]).unwrap();
        assert!(
            prior["params"]["effortRevision"].as_u64() < committed["effortRevision"].as_u64(),
            "queued report cannot undo later effort ACK"
        );
        assert_eq!(
            prior["params"]["modelRevision"],
            json!(a.model_revision),
            "effort does not invalidate an independently learned model"
        );
    }
    #[test]
    fn effort_attempt_preserves_epoch_and_validates_target_before_mutation() {
        let mut a = Adapter::new("session".into()).unwrap();
        a.request(1, "initialize", &json!({})).unwrap();
        ack(
            &mut a,
            1,
            json!({"models":[{"value":"default","resolvedModel":"native","supportsEffort":true,"supportedEffortLevels":["high"]},{"value":"legacy"}]}),
        );
        a.request(2, "effort/probe", &json!({})).unwrap();
        let probe = ack(&mut a, 2, json!({}));
        a.commit_effort(&probe[0]["result"]).unwrap();
        assert!(a
            .request(
                3,
                "settings/validate",
                &json!({"model":"legacy","reasoningEffort":"high"})
            )
            .is_err());
        assert_eq!(a.settings_epoch, 0);
        a.request(
            4,
            "turn/start",
            &json!({"input":[{"type":"text","text":"A"}]}),
        )
        .unwrap();
        a.request(5, "effort/set", &json!({"reasoningEffort":"high"}))
            .unwrap();
        a.unknown_effort();
        a.forget_control(5);
        let old = a
            .consume(json!({"type":"system","subtype":"init","model":"old"}))
            .unwrap();
        assert!(!old.iter().any(|e| e["method"] == "thread/model/updated"));
        assert!(!a.effort_known);
        assert!(a
            .request(
                6,
                "settings/validate",
                &json!({"model":null,"reasoningEffort":"high"})
            )
            .is_err());
    }
}
