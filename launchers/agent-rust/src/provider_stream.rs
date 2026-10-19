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
    windows: HashMap<u64, Window>,
    recent: Window,
    total: usize,
    active: Option<Arc<Value>>,
    targets: HashMap<u64, Arc<Value>>,
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
            windows: HashMap::new(),
            recent: Window::default(),
            total: 0,
            active: None,
            targets: HashMap::new(),
        }
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
            return Err("line_too_large");
        }
        if !self.in_history {
            self.outside += bytes;
        }
        if self.outside > 4 * 1024 * 1024
            || self
                .turn_begin
                .is_some_and(|start| self.count - start > WINDOW_BYTES)
        {
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
            if depth > 32 {
                return Err("provider_json_limit");
            }
            self.nodes += 1;
            self.total_nodes += 1;
            if self.nodes > if self.in_history { 500_000 } else { 50_000 } {
                return Err("provider_json_limit");
            }
            self.spaces().await?;
            if path == ["result", "thread", "turns"] && self.peek().await? != Some(b'[') {
                return Err("provider_json_limit");
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
                            return Err("provider_json_limit");
                        }
                        self.nodes += 1;
                        self.total_nodes += 1;
                        self.spaces().await?;
                        if self.byte().await? != b':' {
                            return Err("invalid_json");
                        }
                        let mut child = path.clone();
                        child.push(key.clone());
                        let value = self.value(depth + 1, child).await?;
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
        self.in_history = true;
        self.byte().await?;
        self.spaces().await?;
        let selections = self.selections.lock().unwrap().clone();
        if selections.len() > 4 {
            return Err("provider_json_limit");
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
            self.nodes = 0;
            let begin = self.count;
            self.turn_begin = Some(begin);
            let value = self.value(depth + 1, vec!["#turn".into()]).await?;
            self.turn_begin = None;
            if self.count - begin > WINDOW_BYTES {
                return Err("provider_value_too_large");
            }
            let bytes = serde_json::to_vec(&value)
                .map_err(|_| "invalid_json")?
                .len();
            let value = Arc::new(value);
            if value["status"] == "inProgress" {
                if self.active.is_some() {
                    return Err("provider_json_limit");
                }
                self.active = Some(value.clone());
            }
            for (&id, selection) in &selections {
                if selection
                    .target
                    .as_deref()
                    .is_some_and(|target| value["id"] == target)
                    && self.targets.insert(id, value.clone()).is_some()
                {
                    return Err("provider_json_limit");
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
                return Err("provider_json_limit");
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
        tokio::time::timeout(std::time::Duration::from_secs(60), self.frame())
            .await
            .map_err(|_| "provider_frame_timeout")?
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
        self.targets.clear();
        self.spaces().await?;
        if self.peek().await?.is_none() {
            return Ok(None);
        }
        let mut value = self.value(0, Vec::new()).await?;
        self.spaces().await?;
        match self.peek().await? {
            Some(b'\n') => {
                self.pos += 1;
            }
            None => {}
            _ => return Err("invalid_json"),
        }
        if value["result"]["thread"]["turns"].is_array() {
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
                        .len();
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
                    .map(|v| Arc::try_unwrap(v).unwrap_or_else(|v| (*v).clone()))
                    .collect(),
            );
            value["result"]["treerHistory"] = serde_json::json!({"total":self.total,"start":window.start,"frameBytes":self.count,"nodeCount":self.total_nodes,"elapsedMs":started.elapsed().as_millis()});
        }
        Ok(Some(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[tokio::test]
    async fn windows_are_selected_even_if_result_precedes_response_id() {
        let selections = Selections::default();
        selections.lock().unwrap().insert(
            7,
            Selection {
                range: Some((10, 20)),
                target: None,
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
