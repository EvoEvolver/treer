//! Streaming provider JSON lines. A history response is scanned once, retaining
//! only bounded windows, never the complete wire line or complete turns array.
use serde_json::{Map, Value};
use std::{
    collections::{HashMap, VecDeque},
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};
use tokio::io::{AsyncRead, AsyncReadExt};

pub const FRAME_BYTES: usize = 256 * 1024 * 1024;
pub const WINDOW_BYTES: usize = 16 * 1024 * 1024;
pub const WINDOW_TURNS: usize = 50;
#[derive(Clone, Default)]
pub struct Selection {
    pub range: Option<(usize, usize)>,
    pub target: Option<String>,
    pub full_history: bool,
}
pub type Selections = Arc<Mutex<HashMap<u64, Selection>>>;
type Result<T> = std::result::Result<T, &'static str>;
type Parsing<'a> = Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>>;

pub struct Reader<R> {
    input: R,
    buffer: [u8; 8192],
    pos: usize,
    end: usize,
    count: usize,
    nodes: usize,
    total_nodes: usize,
    outside: usize,
    in_history: bool,
    turn_begin: Option<usize>,
    selections: Selections,
    response_filter: Option<Box<dyn Fn(u64) -> Option<Selection> + Send + Sync>>,
    windows: HashMap<u64, Window>,
    recent: Window,
    total: usize,
    active: Option<Arc<Value>>,
    active_count: usize,
    targets: HashMap<u64, Arc<Value>>,
    max_depth: usize,
    frame_kind: &'static str,
    limit_kind: &'static str,
    outside_nodes: usize,
    history_seen: bool,
}
#[derive(Default)]
struct Window {
    values: VecDeque<Arc<Value>>,
    charges: VecDeque<(usize, usize)>,
    bytes: usize,
    nodes: usize,
    start: usize,
}
pub(crate) fn shape_nodes(value: &Value) -> usize {
    1 + match value {
        Value::Array(v) => v.iter().map(shape_nodes).sum::<usize>(),
        Value::Object(v) => v.len() + v.values().map(shape_nodes).sum::<usize>(),
        _ => 0,
    }
}
impl Window {
    fn push(&mut self, value: Arc<Value>, bytes: usize, index: usize) -> Result<()> {
        if bytes > WINDOW_BYTES {
            return Err("provider_value_too_large");
        }
        self.bytes += bytes;
        let nodes = shape_nodes(&value);
        self.nodes += nodes;
        self.charges.push_back((bytes, nodes));
        self.values.push_back(value);
        while self.values.len() > WINDOW_TURNS || self.bytes > WINDOW_BYTES || self.nodes > 500_000
        {
            self.values.pop_front().unwrap();
            let (bytes, nodes) = self.charges.pop_front().unwrap();
            self.bytes -= bytes;
            self.nodes -= nodes;
            self.start += 1;
        }
        self.start = index + 1 - self.values.len();
        Ok(())
    }
}
impl<R: AsyncRead + Unpin + Send> Reader<R> {
    pub fn new(input: R, selections: Selections) -> Self {
        Self {
            input,
            buffer: [0; 8192],
            pos: 0,
            end: 0,
            count: 0,
            nodes: 0,
            total_nodes: 0,
            outside: 0,
            in_history: false,
            turn_begin: None,
            selections,
            response_filter: None,
            windows: HashMap::new(),
            recent: Window::default(),
            total: 0,
            active: None,
            active_count: 0,
            targets: HashMap::new(),
            max_depth: 0,
            frame_kind: "responseOrUnknown",
            limit_kind: "none",
            outside_nodes: 0,
            history_seen: false,
        }
    }
    pub fn with_response_filter(
        mut self,
        active: impl Fn(u64) -> Option<Selection> + Send + Sync + 'static,
    ) -> Self {
        self.response_filter = Some(Box::new(active));
        self
    }
    fn shape_limit(&mut self, kind: &'static str) -> &'static str {
        self.limit_kind = kind;
        "provider_json_limit"
    }
    pub fn diagnostic(&self) -> Value {
        serde_json::json!({"frameBytes":self.count,"outsideBytes":self.outside,"nodeCount":self.total_nodes,"shapeNodes":self.nodes,"outsideNodes":self.outside_nodes,"maxDepth":self.max_depth,"turnCount":self.total,"inHistory":self.in_history,"frameKind":self.frame_kind,"limitKind":self.limit_kind,"inProgressCount":self.active_count})
    }
    fn outside_budget(&self) -> (usize, usize) {
        // Until the root method is read, use a bounded provisional envelope.
        // A late control method is rechecked before any frame is returned.
        if self.history_seen || matches!(self.frame_kind, "other" | "thread/tokenUsage/updated") {
            (4 * 1024 * 1024, 50_000)
        } else {
            (WINDOW_BYTES, 500_000)
        }
    }
    fn check_outside(&mut self) -> Result<()> {
        let (bytes, nodes) = self.outside_budget();
        if self.outside > bytes {
            self.limit_kind = "outsideBytes";
            return Err("provider_value_too_large");
        }
        if self.outside_nodes > nodes {
            return Err(self.shape_limit("nodes"));
        }
        Ok(())
    }
    async fn peek(&mut self) -> Result<Option<u8>> {
        if self.pos == self.end {
            self.end = self
                .input
                .read(&mut self.buffer)
                .await
                .map_err(|_| "line_error")?;
            self.pos = 0;
            if self.end == 0 {
                return Ok(None);
            }
        }
        Ok(Some(self.buffer[self.pos]))
    }
    async fn byte(&mut self) -> Result<u8> {
        let byte = self.peek().await?.ok_or("invalid_json")?;
        self.consume(1)?;
        Ok(byte)
    }
    fn consume(&mut self, bytes: usize) -> Result<()> {
        self.pos += bytes;
        self.count += bytes;
        if self.count > FRAME_BYTES {
            self.limit_kind = "frameBytes";
            return Err("line_too_large");
        }
        if !self.in_history {
            self.outside += bytes;
        }
        self.check_outside()?;
        if self
            .turn_begin
            .is_some_and(|start| self.count - start > WINDOW_BYTES)
        {
            self.limit_kind = "turnBytes";
            return Err("provider_value_too_large");
        }
        Ok(())
    }
    async fn spaces(&mut self) -> Result<()> {
        while matches!(self.peek().await?, Some(b' ' | b'\t' | b'\r')) {
            self.byte().await?;
        }
        Ok(())
    }
    async fn string(&mut self, limit: usize) -> Result<String> {
        if self.byte().await? != b'"' {
            return Err("invalid_json");
        }
        let mut bytes = vec![b'"'];
        let mut escape = false;
        loop {
            // Copy ordinary string spans from the read buffer in chunks;
            // reserve per-byte parsing for quotes, escapes and control bytes.
            // Wire/value budgets are checked before extending the allocation.
            if !escape && self.peek().await?.is_some() {
                let begin = self.pos;
                let plain = self.buffer[begin..self.end]
                    .iter()
                    .take_while(|b| **b >= 0x20 && **b != b'"' && **b != b'\\')
                    .count();
                if plain > 0 {
                    self.consume(plain)?;
                    if bytes.len() + plain > limit {
                        self.limit_kind = "stringBytes";
                        return Err("provider_value_too_large");
                    }
                    bytes.extend_from_slice(&self.buffer[begin..begin + plain]);
                    continue;
                }
            }
            let byte = self.byte().await?;
            if byte == b'\n' {
                return Err("invalid_json");
            }
            bytes.push(byte);
            if bytes.len() > limit {
                self.limit_kind = "stringBytes";
                return Err("provider_value_too_large");
            }
            if byte == b'"' && !escape {
                break;
            }
            escape = byte == b'\\' && !escape;
        }
        serde_json::from_slice(&bytes).map_err(|_| "invalid_json")
    }
    fn value<'a>(&'a mut self, depth: usize, path: Vec<String>) -> Parsing<'a> {
        Box::pin(async move {
            self.max_depth = self.max_depth.max(depth);
            if depth > 32 {
                return Err(self.shape_limit("depth"));
            }
            self.nodes += 1;
            self.total_nodes += 1;
            if !self.in_history {
                self.outside_nodes += 1;
                self.check_outside()?;
            }
            if self.nodes > 500_000 {
                return Err(self.shape_limit("nodes"));
            }
            self.spaces().await?;
            if path == ["result", "thread", "turns"] && self.peek().await? != Some(b'[') {
                return Err(self.shape_limit("historyShape"));
            }
            match self.peek().await?.ok_or("invalid_json")? {
                b'"' => Ok(Value::String(self.string(WINDOW_BYTES).await?)),
                b'{' => {
                    self.byte().await?;
                    self.spaces().await?;
                    let mut values = Map::new();
                    if self.peek().await? == Some(b'}') {
                        self.byte().await?;
                        return Ok(Value::Object(values));
                    }
                    loop {
                        self.spaces().await?;
                        let key = self.string(4096).await?;
                        if values.contains_key(&key) {
                            return Err(self.shape_limit("duplicateKey"));
                        }
                        self.nodes += 1;
                        self.total_nodes += 1;
                        if !self.in_history {
                            self.outside_nodes += 1;
                            self.check_outside()?;
                        }
                        self.spaces().await?;
                        if self.byte().await? != b':' {
                            return Err("invalid_json");
                        }
                        let mut child = path.clone();
                        child.push(key.clone());
                        let value = self.value(depth + 1, child).await?;
                        if path.is_empty() && key == "method" {
                            self.frame_kind = match value.as_str() {
                                Some("turn/started") => "turn/started",
                                Some("turn/completed") => "turn/completed",
                                Some("item/started") => "item/started",
                                Some("item/completed") => "item/completed",
                                Some("turn/plan/updated") => "turn/plan/updated",
                                Some("item/agentMessage/delta") => "item/agentMessage/delta",
                                Some("item/reasoning/textDelta") => "item/reasoning/textDelta",
                                Some("item/reasoning/summaryTextDelta") => {
                                    "item/reasoning/summaryTextDelta"
                                }
                                Some("item/commandExecution/outputDelta") => {
                                    "item/commandExecution/outputDelta"
                                }
                                Some("thread/tokenUsage/updated") => "thread/tokenUsage/updated",
                                _ => "other",
                            };
                            self.check_outside()?;
                        }
                        values.insert(key, value);
                        self.spaces().await?;
                        match self.byte().await? {
                            b'}' => break,
                            b',' => {}
                            _ => return Err("invalid_json"),
                        }
                    }
                    Ok(Value::Object(values))
                }
                b'[' => {
                    if path == ["result", "thread", "turns"] {
                        return self.turns(depth).await;
                    }
                    self.byte().await?;
                    self.spaces().await?;
                    let mut values = Vec::new();
                    let begin = self.count;
                    if self.peek().await? == Some(b']') {
                        self.byte().await?;
                        return Ok(Value::Array(values));
                    }
                    loop {
                        values.push(self.value(depth + 1, path.clone()).await?);
                        if self.count - begin > WINDOW_BYTES {
                            self.limit_kind = "arrayBytes";
                            return Err("provider_value_too_large");
                        }
                        self.spaces().await?;
                        match self.byte().await? {
                            b']' => break,
                            b',' => {}
                            _ => return Err("invalid_json"),
                        }
                    }
                    Ok(Value::Array(values))
                }
                _ => {
                    let mut token = Vec::new();
                    while let Some(b) = self.peek().await? {
                        if matches!(b, b',' | b']' | b'}' | b' ' | b'\t' | b'\r' | b'\n') {
                            break;
                        }
                        token.push(self.byte().await?);
                        if token.len() > 128 {
                            return Err("invalid_json");
                        }
                    }
                    serde_json::from_slice(&token).map_err(|_| "invalid_json")
                }
            }
        })
    }
    async fn turns(&mut self, depth: usize) -> Result<Value> {
        self.history_seen = true;
        self.check_outside()?;
        self.in_history = true;
        self.byte().await?;
        self.spaces().await?;
        let selections = self.selections.lock().unwrap().clone();
        if selections.len() > 4 {
            return Err(self.shape_limit("selectionCount"));
        }
        for (&id, selection) in &selections {
            if let Some((start, _)) = selection.range {
                self.windows.insert(
                    id,
                    Window {
                        start,
                        ..Window::default()
                    },
                );
            }
        }
        if self.peek().await? == Some(b']') {
            self.byte().await?;
            self.in_history = false;
            return Ok(Value::Array(Vec::new()));
        }
        loop {
            // Per-turn shape bound applies even to discarded history. Total
            // scan work additionally has the frame-byte ceiling and RPC deadline.
            if self.total == 100_000 {
                return Err(self.shape_limit("turnCount"));
            }
            self.nodes = 0;
            let begin = self.count;
            self.turn_begin = Some(begin);
            let mut value = self.value(depth + 1, vec!["#turn".into()]).await?;
            self.turn_begin = None;
            if self.count - begin > WINDOW_BYTES {
                return Err("provider_value_too_large");
            }
            // Reserve provenance bytes/nodes in every retained window BEFORE
            // admission; only a locally rewritten status keeps this marker.
            if let Some(object) = value.as_object_mut() {
                object.remove("treerStatusProjected");
            }
            if value["status"] == "inProgress" {
                value["treerStatusProjected"] = Value::Bool(true);
            }
            let bytes = serde_json::to_vec(&value)
                .map_err(|_| "invalid_json")?
                .len()
                + usize::from(value["status"] == "inProgress"); // interrupted is one byte longer
            let value = Arc::new(value);
            if value["status"] == "inProgress" {
                self.active_count += 1;
                self.active = Some(value.clone());
            }
            for (&id, selection) in &selections {
                if selection
                    .target
                    .as_deref()
                    .is_some_and(|target| value["id"] == target)
                    && self.targets.insert(id, value.clone()).is_some()
                {
                    return Err(self.shape_limit("duplicateTarget"));
                }
                if selection
                    .range
                    .is_some_and(|(start, end)| self.total >= start && self.total < end)
                {
                    self.windows
                        .entry(id)
                        .or_default()
                        .push(value.clone(), bytes, self.total)?;
                }
            }
            self.recent.push(value, bytes, self.total)?;
            self.total += 1;
            if self.total > 100_000 {
                return Err(self.shape_limit("turnCount"));
            }
            self.spaces().await?;
            match self.byte().await? {
                b']' => break,
                b',' => {}
                _ => return Err("invalid_json"),
            }
        }
        self.in_history = false;
        self.nodes = 0;
        Ok(Value::Array(Vec::new()))
    }
    pub async fn next(&mut self) -> Result<Option<Value>> {
        if self.peek().await?.is_none() {
            return Ok(None);
        }
        match tokio::time::timeout(std::time::Duration::from_secs(60), self.frame()).await {
            Ok(result) => result,
            Err(_) => {
                self.limit_kind = "frameDeadline";
                Err("provider_frame_timeout")
            }
        }
    }
    async fn frame(&mut self) -> Result<Option<Value>> {
        let started = std::time::Instant::now();
        self.count = 0;
        self.nodes = 0;
        self.total_nodes = 0;
        self.outside = 0;
        self.in_history = false;
        self.turn_begin = None;
        self.windows.clear();
        self.recent = Window::default();
        self.total = 0;
        self.active = None;
        self.active_count = 0;
        self.targets.clear();
        self.max_depth = 0;
        self.frame_kind = "responseOrUnknown";
        self.limit_kind = "none";
        self.outside_nodes = 0;
        self.history_seen = false;
        self.spaces().await?;
        if self.peek().await?.is_none() {
            return Ok(None);
        }
        let mut value = self.value(0, Vec::new()).await?;
        self.check_outside()?;
        self.spaces().await?;
        match self.peek().await? {
            Some(b'\n') => {
                self.pos += 1;
            }
            None => {}
            _ => return Err("invalid_json"),
        }
        // A fully parsed late/unknown response cannot confer history authority
        // or poison a newer request. Grammar, duplicate keys and streaming
        // byte/depth/node/frame limits above still apply to the entire frame.
        // Notifications/control requests are never skipped by this filter.
        let mut admitted_selection = None;
        if value.get("method").is_none() {
            if let (Some(active), Some(id)) = (&self.response_filter, value["id"].as_u64()) {
                match active(id) {
                    Some(selection) => admitted_selection = Some(selection),
                    None => {
                        self.windows.clear();
                        self.targets.clear();
                        self.active = None;
                        self.recent = Window::default();
                        return Ok(Some(serde_json::json!({"id":id})));
                    }
                }
            }
        }
        if value["result"]["thread"]["turns"].is_array() {
            // Codex history can retain inProgress after a provider restart.
            // Match Node's ordered full-history interpretation only with an
            // explicit usable thread status. Metadata may follow the turns.
            let status = match value["result"]["thread"]["status"]["type"].as_str() {
                Some("idle") => Some("idle"),
                Some("active") => Some("active"),
                None => None,
                _ => Some("other"),
            };
            let selection = admitted_selection.unwrap_or_else(|| {
                self.selections
                    .lock()
                    .unwrap()
                    .get(&value["id"].as_u64().unwrap_or(0))
                    .cloned()
                    .unwrap_or_default()
            });
            let full_history = selection.full_history;
            // With several in-progress turns the newest is the live one (none
            // when the thread reports idle); older ones are shown interrupted.
            if self.active_count > 1 {
                eprintln!("Treer launcher: provider history has {} in-progress turns; following the newest", self.active_count);
            }
            let normalize = self.active_count > 1
                || (full_history
                    && self.active_count > 0
                    && matches!(status, Some("idle" | "active")));
            let live_id = if status == Some("idle") && normalize {
                None
            } else {
                self.active
                    .as_ref()
                    .and_then(|t| t["id"].as_str())
                    .map(str::to_owned)
            };
            if normalize && live_id.is_none() {
                self.active = None;
            }
            let mut live_turn = self.active.clone();
            let id = value["id"].as_u64().unwrap_or(0);
            let selected = self.windows.contains_key(&id);
            let mut window = self
                .windows
                .remove(&id)
                .unwrap_or_else(|| std::mem::take(&mut self.recent));
            if !selected {
                if let Some(active) = self
                    .targets
                    .remove(&id)
                    .or_else(|| self.active.take())
                    .filter(|active| !window.values.iter().any(|t| t["id"] == active["id"]))
                {
                    let size = serde_json::to_vec(&*active)
                        .map_err(|_| "invalid_json")?
                        .len()
                        + usize::from(active["status"] == "inProgress");
                    let nodes = shape_nodes(&active);
                    while window.values.len() >= WINDOW_TURNS
                        || window.bytes + size > WINDOW_BYTES
                        || window.nodes + nodes > 500_000
                    {
                        window
                            .values
                            .pop_front()
                            .ok_or("provider_value_too_large")?;
                        let (bytes, nodes) = window
                            .charges
                            .pop_front()
                            .ok_or("provider_value_too_large")?;
                        window.bytes -= bytes;
                        window.nodes -= nodes;
                        window.start += 1;
                    }
                    window.values.push_front(active);
                }
            }
            // Other selection ranges share the same turn allocations. Drop
            // them before materializing the one response for this RPC.
            self.windows.clear();
            self.targets.clear();
            self.active = None;
            self.recent = Window::default();
            value["result"]["thread"]["turns"] = Value::Array(
                window
                    .values
                    .into_iter()
                    .map(|v| {
                        let is_live = live_turn
                            .as_ref()
                            .is_some_and(|active| Arc::ptr_eq(active, &v));
                        if is_live {
                            live_turn = None;
                        }
                        let mut turn = Arc::try_unwrap(v).unwrap_or_else(|v| (*v).clone());
                        if normalize && turn["status"] == "inProgress" && !is_live {
                            turn["status"] = Value::String("interrupted".into());
                            // Marks a status Treer inferred, not one the provider sent.
                            turn["treerStatusProjected"] = Value::Bool(true);
                        } else if let Some(object) = turn.as_object_mut() {
                            object.remove("treerStatusProjected");
                        }
                        turn
                    })
                    .collect(),
            );
            value["result"]["treerHistory"] = serde_json::json!({"total":self.total,"start":window.start,"frameBytes":self.count,"nodeCount":self.total_nodes,"elapsedMs":started.elapsed().as_millis(),"staleInProgressCount":if normalize {self.active_count-usize::from(live_id.is_some())} else {0},"threadStatus":match status {Some("idle")=>"idle",Some("active")=>"active",None=>"missing",_=>"other"}});
        }
        Ok(Some(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[tokio::test]
    async fn expired_response_skips_history_but_not_wire_limits_or_current_responses() {
        let history = serde_json::to_vec(&json!({"id":1,"result":{"thread":{"status":{"type":"idle"},"turns":[{"id":"old","status":"inProgress"},{"id":"recent","status":"inProgress"}]}}})).unwrap();
        let ignored = Reader::new(history.as_slice(), Selections::default())
            .with_response_filter(|_| None)
            .next()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(ignored, json!({"id":1}));
        // Several in-progress turns in an idle thread are all stale.
        let admitted = Reader::new(history.as_slice(), Selections::default())
            .with_response_filter(|_| Some(Selection::default()))
            .next()
            .await
            .unwrap()
            .unwrap();
        assert!(admitted["result"]["thread"]["turns"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["status"] == "interrupted"));
        for malformed in [
            b"{\"id\":1,\"result\":1,\"result\":2}\n".as_slice(),
            b"{\"id\":1,\"result\":[}\n".as_slice(),
        ] {
            assert!(Reader::new(malformed, Selections::default())
                .with_response_filter(|_| None)
                .next()
                .await
                .is_err());
        }
        let deep = format!(
            "{{\"id\":1,\"result\":{}{}}}\n",
            "[".repeat(40),
            "]".repeat(40)
        );
        assert!(Reader::new(deep.as_bytes(), Selections::default())
            .with_response_filter(|_| None)
            .next()
            .await
            .is_err());
        let control =
            b"{\"id\":1,\"method\":\"item/commandExecution/requestApproval\",\"params\":{}}\n";
        assert_eq!(
            Reader::new(control.as_slice(), Selections::default())
                .with_response_filter(|_| None)
                .next()
                .await
                .unwrap()
                .unwrap()["method"],
            "item/commandExecution/requestApproval"
        );
        let selections = Selections::default();
        selections.lock().unwrap().insert(
            1,
            Selection {
                full_history: true,
                ..Selection::default()
            },
        );
        let cancelled = selections.clone();
        let admitted = Reader::new(history.as_slice(), selections)
            .with_response_filter(move |id| {
                // Deterministic check/use interleaving: registry disappears after
                // admission, before the Reader performs history interpretation.
                let captured = cancelled.lock().unwrap().get(&id).cloned();
                cancelled.lock().unwrap().remove(&id);
                captured
            })
            .next()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            admitted["result"]["treerHistory"]["staleInProgressCount"],
            2
        );
    }
    #[tokio::test]
    async fn producer_history_status_normalizes_stale_turns_after_complete_metadata() {
        for status in ["idle", "active"] {
            for late in [false, true] {
                let turns = json!([
                    {"id":"stale","status":"inProgress"},
                    {"id":"finished","status":"completed"},
                    {"id":"newest","status":"inProgress"}
                ]);
                let metadata = format!("\"status\":{{\"type\":\"{status}\",\"activeFlags\":[]}}");
                let thread = if late {
                    format!("\"turns\":{turns},{metadata}")
                } else {
                    format!("{metadata},\"turns\":{turns}")
                };
                let wire = format!("{{\"id\":1,\"result\":{{\"thread\":{{{thread}}}}}}}\n");
                let selections = Selections::default();
                selections.lock().unwrap().insert(
                    1,
                    Selection {
                        full_history: true,
                        ..Selection::default()
                    },
                );
                let result = Reader::new(wire.as_bytes(), selections)
                    .next()
                    .await
                    .unwrap()
                    .unwrap();
                let turns = result["result"]["thread"]["turns"].as_array().unwrap();
                assert_eq!(turns[0]["status"], "interrupted");
                assert_eq!(turns[0]["treerStatusProjected"], true);
                assert_eq!(turns[1]["status"], "completed");
                assert!(turns[1].get("treerStatusProjected").is_none());
                assert_eq!(
                    turns[2]["status"],
                    if status == "idle" {
                        "interrupted"
                    } else {
                        "inProgress"
                    }
                );
                assert_eq!(turns[2]["treerStatusProjected"] == true, status == "idle");
            }
        }
        for status in [
            Value::Null,
            json!({"type":"unknown"}),
            json!({"type":"systemError"}),
        ] {
            let wire =
                serde_json::to_vec(&json!({"id":1,"result":{"thread":{"status":status,"turns":[
                    {"id":"one","status":"inProgress"},{"id":"two","status":"inProgress"}
                ]}}}))
                .unwrap();
            // Without a usable status the newest in-progress turn is live.
            let value = Reader::new(wire.as_slice(), Selections::default())
                .next()
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                value["result"]["thread"]["turns"][0]["status"],
                "interrupted"
            );
            assert_eq!(
                value["result"]["thread"]["turns"][1]["status"],
                "inProgress"
            );
        }
    }
    #[tokio::test]
    async fn projection_only_does_not_weaken_target_or_request_authority() {
        let wire = br#"{"id":1,"result":{"thread":{"status":{"type":"idle"},"turns":[{"id":"same","status":"completed"},{"id":"same","status":"completed"}]}}}"#;
        let selections = Selections::default();
        selections.lock().unwrap().insert(
            1,
            Selection {
                full_history: true,
                target: Some("same".into()),
                ..Default::default()
            },
        );
        let mut reader = Reader::new(wire.as_slice(), selections);
        assert_eq!(reader.next().await.unwrap_err(), "provider_json_limit");
        assert_eq!(reader.diagnostic()["limitKind"], "duplicateTarget");
        for candidates in [false, true] {
            // A response without matched full-request authority does not
            // acquire it from the mere presence of an idle status.
            let wire = serde_json::to_vec(&json!({"id":1,"result":{"thread":{"status":{"type":"idle"},"turns":[{"id":"one","status":if candidates {"inProgress"} else {"completed"}}]}}})).unwrap();
            let mut reader = Reader::new(wire.as_slice(), Selections::default());
            let value = reader.next().await.unwrap().unwrap();
            assert_eq!(
                value["result"]["thread"]["turns"][0]["status"],
                if candidates {
                    "inProgress"
                } else {
                    "completed"
                }
            );
        }
    }
    #[tokio::test]
    async fn ambiguous_or_paged_history_follows_the_newest_in_progress_turn() {
        for case in ["unregistered", "paged", "duplicate", "reordered"] {
            let mut thread = json!({"status":{"type":"active"},"turns":[
                {"id":"one","status":"inProgress","startedAt":10},
                {"id":"two","status":"inProgress","startedAt":20}
            ]});
            match case {
                "paged" => thread["nextCursor"] = json!("synthetic-next"),
                "duplicate" => thread["turns"][1]["id"] = json!("one"),
                "reordered" => thread["turns"][1]["startedAt"] = json!(9),
                _ => {}
            }
            let bytes = serde_json::to_vec(&json!({"id":1,"result":{"thread":thread}})).unwrap();
            let selections = Selections::default();
            if case != "unregistered" {
                selections.lock().unwrap().insert(
                    1,
                    Selection {
                        full_history: true,
                        ..Default::default()
                    },
                );
            }
            let value = Reader::new(bytes.as_slice(), selections)
                .next()
                .await
                .unwrap()
                .unwrap();
            let turns = value["result"]["thread"]["turns"].as_array().unwrap();
            assert_eq!(turns[0]["status"], "interrupted", "{case}");
            assert_eq!(turns[1]["status"], "inProgress", "{case}");
        }
    }
    #[tokio::test]
    async fn producer_52_turn_node_rich_history_keeps_existing_limits() {
        let turns = (0..52).map(|n| json!({"id":format!("synthetic-{n}"),"startedAt":n,"status":"inProgress",
            "items":[{"type":"dynamicToolCall","output":"x".repeat(230*1024),"content":vec![json!({"a":[0,1,2],"b":true});210]}]})).collect::<Vec<_>>();
        let wire = serde_json::to_vec(
            &json!({"id":1,"result":{"thread":{"turns":turns,"status":{"type":"idle"}}}}),
        )
        .unwrap();
        assert!(wire.len() > 11 * 1024 * 1024 && wire.len() < 13 * 1024 * 1024);
        let selections = Selections::default();
        selections.lock().unwrap().insert(
            1,
            Selection {
                full_history: true,
                ..Default::default()
            },
        );
        let result = Reader::new(wire.as_slice(), selections)
            .next()
            .await
            .unwrap()
            .unwrap();
        assert!(
            result["result"]["treerHistory"]["nodeCount"]
                .as_u64()
                .unwrap()
                > 86_000
        );
        assert_eq!(result["result"]["treerHistory"]["staleInProgressCount"], 52);
        assert!(result["result"]["thread"]["turns"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["status"] == "interrupted"));
    }
    #[tokio::test]
    async fn history_turn_count_cap_refuses_instead_of_truncating() {
        let turns = (0..100_001)
            .map(|n| json!({"id":n.to_string(),"status":"completed"}))
            .collect::<Vec<_>>();
        let bytes = serde_json::to_vec(
            &json!({"id":1,"result":{"thread":{"turns":turns,"status":{"type":"idle"}}}}),
        )
        .unwrap();
        let mut reader = Reader::new(bytes.as_slice(), Selections::default());
        assert_eq!(reader.next().await.unwrap_err(), "provider_json_limit");
        assert_eq!(reader.diagnostic()["limitKind"], "turnCount");
    }
    #[tokio::test]
    async fn bounded_live_tool_shapes_and_content_free_failure_diagnostics() {
        for count in [8_000, 80_000] {
            let bytes = serde_json::to_vec(&json!({"method":"item/completed","params":{"item":{"content":vec![json!({"a":[0,1,2],"b":true});count]}}})).unwrap();
            assert!(bytes.len() < 4 * 1024 * 1024);
            let mut reader = Reader::new(bytes.as_slice(), Selections::default());
            let result = reader.next().await;
            if count == 8_000 {
                assert!(result.unwrap().is_some());
                assert!(reader.diagnostic()["nodeCount"].as_u64().unwrap() > 50_000);
            } else {
                assert_eq!(result.unwrap_err(), "provider_json_limit");
                let diagnostic = reader.diagnostic();
                assert_eq!(diagnostic["limitKind"], "nodes");
                assert_eq!(diagnostic["frameKind"], "item/completed");
                assert!(diagnostic["shapeNodes"].as_u64().unwrap() > 500_000);
                assert!(diagnostic["frameBytes"].as_u64().unwrap() < 4 * 1024 * 1024);
            }
        }
        let bytes = format!(
            "{{\"method\":\"SYNTHETIC_SECRET\",\"params\":{}0{}}}",
            "[".repeat(40),
            "]".repeat(40)
        );
        let mut reader = Reader::new(bytes.as_bytes(), Selections::default());
        assert_eq!(reader.next().await.unwrap_err(), "provider_json_limit");
        assert_eq!(reader.diagnostic()["limitKind"], "depth");
        assert!(!reader.diagnostic().to_string().contains("SYNTHETIC_SECRET"));
        for late in [false, true] {
            // A nested allowlisted method never upgrades the root class.
            let payload=serde_json::to_string(&json!({"method":"item/completed","entries":vec![json!({"a":[0,1,2],"b":true});8_000]})).unwrap();
            let bytes = if late {
                format!("{{\"params\":{payload},\"method\":\"secret-control\"}}")
            } else {
                format!("{{\"method\":\"secret-control\",\"params\":{payload}}}")
            };
            let mut reader = Reader::new(bytes.as_bytes(), Selections::default());
            assert_eq!(reader.next().await.unwrap_err(), "provider_json_limit");
            assert_eq!(reader.diagnostic()["frameKind"], "other");
            assert_eq!(reader.diagnostic()["limitKind"], "nodes");
        }
    }
    #[tokio::test]
    async fn windows_are_selected_even_if_result_precedes_response_id() {
        let selections = Selections::default();
        selections.lock().unwrap().insert(
            7,
            Selection {
                range: Some((10, 20)),
                target: None,
                ..Selection::default()
            },
        );
        let turns = (0..100)
            .map(|n| json!({"id":format!("t{n}"),"items":[]}))
            .collect::<Vec<_>>();
        let bytes = format!(
            "{{\"result\":{{\"thread\":{{\"turns\":{}}}}},\"id\":7}}\n",
            serde_json::to_string(&turns).unwrap()
        );
        let value = Reader::new(bytes.as_bytes(), selections)
            .next()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(value["result"]["thread"]["turns"][0]["id"], "t10");
        assert_eq!(
            value["result"]["thread"]["turns"].as_array().unwrap().len(),
            10
        );
        assert_eq!(value["result"]["treerHistory"]["total"], 100);
    }
    #[tokio::test]
    async fn malformed_or_duplicate_keys_in_discarded_history_are_rejected() {
        for bad in [
            "{\"id\":\"old\",\"id\":\"duplicate\"}",
            "{\"id\":bad}",
            "{\"id\":\"unterminated}",
        ] {
            let later = (0..60)
                .map(|n| json!({"id":n.to_string(),"items":[]}))
                .collect::<Vec<_>>();
            let bytes = format!(
                "{{\"id\":1,\"result\":{{\"thread\":{{\"turns\":[{bad},{}]}}}}}}\n",
                serde_json::to_string(&later)
                    .unwrap()
                    .trim_matches(['[', ']'])
            );
            assert!(Reader::new(bytes.as_bytes(), Selections::default())
                .next()
                .await
                .is_err());
        }
    }
    #[tokio::test]
    async fn per_turn_shape_and_single_value_limits_apply_before_retention() {
        let nested = format!("{}0{}", "[".repeat(40), "]".repeat(40));
        let bytes = format!("{{\"result\":{{\"thread\":{{\"turns\":[{nested}]}}}}}}\n");
        assert_eq!(
            Reader::new(bytes.as_bytes(), Selections::default())
                .next()
                .await
                .unwrap_err(),
            "provider_json_limit"
        );
        let bytes = format!(
            "{{\"method\":\"notice\",\"params\":\"{}\"}}\n",
            "x".repeat(WINDOW_BYTES + 1)
        );
        assert_eq!(
            Reader::new(bytes.as_bytes(), Selections::default())
                .next()
                .await
                .unwrap_err(),
            "provider_value_too_large"
        );
    }
    #[tokio::test]
    async fn active_turn_outside_recent_window_is_preserved() {
        let mut turns = vec![json!({"id":"active","status":"inProgress"})];
        turns.extend((0..60).map(|n| json!({"id":n.to_string(),"status":"completed"})));
        let bytes =
            serde_json::to_vec(&json!({"id":1,"result":{"thread":{"turns":turns}}})).unwrap();
        let value = Reader::new(bytes.as_slice(), Selections::default())
            .next()
            .await
            .unwrap()
            .unwrap();
        let kept = value["result"]["thread"]["turns"].as_array().unwrap();
        assert_eq!(kept.len(), 50);
        assert_eq!(kept[0]["id"], "active");
        assert_eq!(kept[49]["id"], "59");
    }

    #[tokio::test]
    async fn completed_reconciliation_target_is_kept_within_the_window_budget() {
        let selections = Selections::default();
        selections.lock().unwrap().insert(
            7,
            Selection {
                range: None,
                target: Some("target".into()),
                ..Selection::default()
            },
        );
        let mut turns = vec![json!({"id":"target","status":"completed"})];
        turns.extend((0..80).map(|n| json!({"id":format!("later-{n}"),"status":"completed"})));
        let bytes =
            serde_json::to_vec(&json!({"id":7,"result":{"thread":{"turns":turns}}})).unwrap();
        let result = Reader::new(bytes.as_slice(), selections)
            .next()
            .await
            .unwrap()
            .unwrap();
        let kept = result["result"]["thread"]["turns"].as_array().unwrap();
        assert_eq!(kept.len(), 50);
        assert_eq!(kept[0]["id"], "target");
        assert_eq!(kept[49]["id"], "later-79");
        assert!(shape_nodes(&result["result"]["thread"]["turns"]) < 500_000);
    }

    #[tokio::test]
    async fn differential_json_grammar_matches_serde_and_never_accepts_rejected_mutations() {
        fn random(seed: &mut u64) -> u64 {
            *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            *seed
        }
        fn document(seed: &mut u64, depth: usize) -> Value {
            match random(seed) % if depth > 3 { 4 } else { 6 } {
                0 => Value::Null,
                1 => json!(random(seed).is_multiple_of(2)),
                2 => json!((random(seed) % 100_000) as i64 - 50_000),
                3 => json!(format!("unicode 🦀 中文 \\ \" \n {}", random(seed))),
                4 => Value::Array(
                    (0..random(seed) % 5)
                        .map(|_| document(seed, depth + 1))
                        .collect(),
                ),
                _ => Value::Object(
                    (0..random(seed) % 5)
                        .map(|n| (format!("key{n}"), document(seed, depth + 1)))
                        .collect(),
                ),
            }
        }
        let mut seed = 158;
        for size in 8180..8200 {
            let value = json!({"text":format!("{}中🦀\\\"\nend","x".repeat(size))});
            let bytes = serde_json::to_vec(&value).unwrap();
            assert_eq!(
                Reader::new(bytes.as_slice(), Selections::default())
                    .next()
                    .await
                    .unwrap(),
                Some(value)
            );
        }
        for _ in 0..500 {
            let value = document(&mut seed, 0);
            let bytes = serde_json::to_vec(&value).unwrap();
            assert_eq!(
                Reader::new(bytes.as_slice(), Selections::default())
                    .next()
                    .await
                    .unwrap(),
                Some(value)
            );
            for end in [0, bytes.len() / 2, bytes.len().saturating_sub(1)] {
                let short = &bytes[..end];
                if serde_json::from_slice::<Value>(short).is_err() {
                    assert!(!matches!(
                        Reader::new(short, Selections::default()).next().await,
                        Ok(Some(_))
                    ));
                }
            }
            let mut mutated = bytes;
            let index = (random(&mut seed) as usize) % mutated.len();
            mutated[index] = b'!';
            if serde_json::from_slice::<Value>(&mutated).is_err() {
                assert!(!matches!(
                    Reader::new(mutated.as_slice(), Selections::default())
                        .next()
                        .await,
                    Ok(Some(_))
                ));
            }
        }
        for invalid in [
            "\"raw\u{1}control\"",
            "\"bad\\q\"",
            "\"\\ud800\"",
            "01",
            "1.",
            "1e+",
            "true false",
            r#"{"result":{"thread":{"turns":[],"turns":[]}}}"#,
        ] {
            assert!(!matches!(
                Reader::new(invalid.as_bytes(), Selections::default())
                    .next()
                    .await,
                Ok(Some(_))
            ));
        }
        let elsewhere = json!({"params":{"turns":[1,2]},"turns":[3,4]});
        let bytes = serde_json::to_vec(&elsewhere).unwrap();
        assert_eq!(
            Reader::new(bytes.as_slice(), Selections::default())
                .next()
                .await
                .unwrap(),
            Some(elsewhere)
        );
        for invalid in [
            r#"{"result":{"thread":{"turns":null}}}"#,
            r#"{"result":{"thread":{"turns":{}}}}"#,
        ] {
            assert!(Reader::new(invalid.as_bytes(), Selections::default())
                .next()
                .await
                .is_err());
        }
    }
}
