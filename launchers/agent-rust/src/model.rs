use serde_json::{json, Value};

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
pub fn preview(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].into()
}
pub fn item(raw: &Value, turn: &str, order: usize) -> Value {
    let ty = raw["type"].as_str().unwrap_or("other");
    let content = raw["content"]
        .as_array()
        .map(|v| {
            v.iter()
                .filter_map(|v| v["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    let text = raw["text"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or(&content);
    let (kind, text, detail) = match ty {
        "userMessage" => ("userMessage", text.to_string(), Value::Null),
        "agentMessage" | "text" => ("agentMessage", text.to_string(), Value::Null),
        "reasoning" => (
            "reasoning",
            [
                raw["summary"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default(),
                text.into(),
            ]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
            Value::Null,
        ),
        "commandExecution" => (
            "commandExecution",
            raw["command"].as_str().unwrap_or(text).into(),
            raw["aggregatedOutput"].clone(),
        ),
        "fileChange" => ("fileChange", "File change".into(), Value::Null),
        "plan" => ("plan", "Plan".into(), Value::Null),
        t if t.ends_with("ToolCall") => (
            "toolCall",
            raw["tool"].as_str().unwrap_or(t).into(),
            json!(raw.to_string()),
        ),
        _ => (
            "other",
            if text.is_empty() { ty } else { text }.into(),
            Value::Null,
        ),
    };
    let mut result = json!({"id":raw["id"].as_str().map(str::to_string).unwrap_or(format!("item-{order}")),"kind":kind,"text":text,"detailText":detail,"status":raw["status"],"transcriptOrder":order,"sourceTurnId":turn,"attachments":[]});
    // A display fallback is not a stable provider identity for reconciliation.
    result["providerAssignedId"] = json!(raw["id"].is_string());
    if kind == "fileChange" {
        result["changes"] = json!(raw["changes"].as_array().unwrap_or(&vec![]).iter().map(|c|json!({"path":c["path"],"diff":c["diff"].as_str().unwrap_or(""),"kind":c["kind"].as_str().or(c["kind"]["type"].as_str()).unwrap_or("update")})).collect::<Vec<_>>());
    }
    if kind == "plan" {
        result["steps"] = json!(raw["plan"]
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .map(|step| {
                json!({"step":step["step"],"status": match step["status"].as_str() {
                    Some("in_progress" | "inProgress") => "inProgress",
                    Some("completed") => "completed",
                    _ => "pending",
                }})
            })
            .collect::<Vec<_>>());
    }
    result
}
pub fn turn(raw: &Value) -> Value {
    let id = raw["id"].as_str().unwrap_or("turn");
    json!({"id":raw["id"],"status":raw["status"].as_str().unwrap_or("completed"),"startedAt":null,"error":raw["error"]["message"],"items":raw["items"].as_array().unwrap_or(&vec![]).iter().enumerate().map(|(n,i)|item(i,id,n)).collect::<Vec<_>>()})
}

#[derive(Debug)]
pub struct InvalidFinalSnapshot;
impl std::fmt::Display for InvalidFinalSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Conflicting provider final snapshot; queue paused")
    }
}
impl std::error::Error for InvalidFinalSnapshot {}

// Only tool-call detail uses this recursive extension rule. Empty/missing
// summary fields retain observed detail; nonempty arrays may extend, never
// shrink. Both inputs already passed the provider depth/node/byte caps and the
// resulting turn must still pass Transcript admission before publication.
fn retained_tool_detail(old: Value, next: Value) -> Result<Value, InvalidFinalSnapshot> {
    let empty = |v: &Value| v.is_null() || v.as_str() == Some("")
        || v.as_array().is_some_and(Vec::is_empty)
        || v.as_object().is_some_and(serde_json::Map::is_empty);
    if empty(&next) { return Ok(old); }
    if empty(&old) { return Ok(next); }
    match (old, next) {
        (Value::Object(mut old), Value::Object(next)) => {
            for (key, value) in next {
                let value = match old.remove(&key) {
                    Some(prior) => retained_tool_detail(prior, value)?,
                    None => value,
                };
                old.insert(key, value);
            }
            Ok(Value::Object(old))
        }
        (Value::Array(old), Value::Array(next)) if next.len() >= old.len() => {
            let mut previous = old.into_iter();
            let merged = next.into_iter().map(|value| match previous.next() {
                Some(prior) => retained_tool_detail(prior, value),
                None => Ok(value),
            }).collect::<Result<Vec<_>, _>>()?;
            Ok(Value::Array(merged))
        }
        (Value::String(old), Value::String(next)) if next.starts_with(&old) => Ok(json!(next)),
        (old, next) if old == next => Ok(next),
        _ => Err(InvalidFinalSnapshot),
    }
}

fn retained_tool_envelope(old: Value, next: Value) -> Result<Value, InvalidFinalSnapshot> {
    // Only these public 0.162.1 result fields may grow. Invocation fields,
    // including type/tool/server/namespace/arguments/input/status/error and
    // unknown metadata, never inherit output-prefix semantics.
    let output_field = match old["type"].as_str() {
        Some("dynamicToolCall") => "contentItems",
        Some("mcpToolCall") => "result",
        _ => "",
    };
    let (Value::Object(mut old), Value::Object(next)) = (old, next) else { return Err(InvalidFinalSnapshot); };
    for (key, value) in next {
        let empty = value.is_null() || value.as_str() == Some("")
            || value.as_array().is_some_and(Vec::is_empty)
            || value.as_object().is_some_and(serde_json::Map::is_empty);
        if let Some(prior) = old.remove(&key) {
            let merged = if key == output_field {
                retained_tool_detail(prior, value)?
            } else if empty || prior == value {
                prior
            } else if prior.is_null() {
                value
            } else { return Err(InvalidFinalSnapshot); };
            old.insert(key, merged);
        } else {
            old.insert(key, value);
        }
    }
    Ok(Value::Object(old))
}

fn item_positions(items: &[Value]) -> Result<std::collections::HashMap<String, usize>, InvalidFinalSnapshot> {
    let mut ids = std::collections::HashMap::new();
    for (n, item) in items.iter().enumerate() {
        let id = item["id"].as_str().filter(|s| !s.is_empty() && s.len() <= 256).ok_or(InvalidFinalSnapshot)?;
        if ids.insert(id.to_owned(), n).is_some() { return Err(InvalidFinalSnapshot); }
    }
    Ok(ids)
}

pub fn final_turn(raw: &Value) -> Result<Value, InvalidFinalSnapshot> {
    if !matches!(raw["status"].as_str(), Some("completed" | "failed" | "interrupted")) {
        return Err(InvalidFinalSnapshot);
    }
    match raw.get("items") {
        Some(Value::Array(items)) => { item_positions(items)?; }
        None | Some(Value::Null) => {}
        _ => return Err(InvalidFinalSnapshot),
    }
    Ok(turn(raw))
}

// Completion notifications and observer summaries need not repeat every item
// already observed live. This is a bounded projection union, not evidence that
// either snapshot contains the complete provider history. Call only after the
// receipt-owned user projection has canonicalized native echoes.
pub fn retain_final_items(old: &Value, next: &mut Value) -> Result<(usize, usize), InvalidFinalSnapshot> {
    let terminal = |status: &Value| matches!(status.as_str(), Some("completed" | "failed" | "interrupted" | "declined"));
    if old["id"] != next["id"]
        || (terminal(&old["status"]) && (old["status"] != next["status"] || old["error"] != next["error"]))
    {
        return Err(InvalidFinalSnapshot);
    }
    let previous = old["items"].as_array().ok_or(InvalidFinalSnapshot)?;
    let incoming = next["items"].as_array().ok_or(InvalidFinalSnapshot)?;
    let positions = item_positions(previous)?;
    item_positions(incoming)?;
    let previous_messages: std::collections::HashSet<_> = previous.iter()
        .filter(|item| matches!(item["kind"].as_str(), Some("userMessage" | "agentMessage")))
        .filter_map(|item| Some((item["kind"].as_str()?, item["text"].as_str().filter(|s| !s.is_empty())?)))
        .collect();
    if previous.iter().chain(incoming).any(|item| item["sourceTurnId"] != next["id"]
        || (item["providerAssignedId"] != true && !item["submissionId"].is_string())) {
        return Err(InvalidFinalSnapshot);
    }
    let mut merged = Vec::with_capacity(previous.len() + incoming.len());
    let mut old_at = 0;
    let mut pending = Vec::new();
    let mut shared = 0;
    for item in incoming {
        if let Some(&at) = positions.get(item["id"].as_str().ok_or(InvalidFinalSnapshot)?) {
            let prior = &previous[at];
            if at < old_at || prior["kind"] != item["kind"]
                || (terminal(&prior["status"]) && prior["status"] != item["status"])
            {
                return Err(InvalidFinalSnapshot);
            }
            merged.extend(previous[old_at..at].iter().cloned());
            merged.append(&mut pending);
            let mut updated = item.clone();
            if terminal(&prior["status"]) && prior["kind"] == "commandExecution" {
                if let Some(output) = prior["detailText"].as_str().filter(|s| !s.is_empty()) {
                    match updated["detailText"].as_str().filter(|s| !s.is_empty()) {
                        None => updated["detailText"] = prior["detailText"].clone(),
                        Some(next_output) if next_output.starts_with(output) => {},
                        _ => return Err(InvalidFinalSnapshot),
                    }
                }
            }
            if terminal(&prior["status"]) && prior["kind"] == "fileChange" {
                let old = prior["changes"].as_array().ok_or(InvalidFinalSnapshot)?;
                let new = updated["changes"].as_array().ok_or(InvalidFinalSnapshot)?;
                if new.is_empty() { updated["changes"] = prior["changes"].clone(); }
                else if !new.starts_with(old) { return Err(InvalidFinalSnapshot); }
            }
            if terminal(&prior["status"]) && prior["kind"] == "toolCall" {
                let old: Value = serde_json::from_str(prior["detailText"].as_str().ok_or(InvalidFinalSnapshot)?).map_err(|_| InvalidFinalSnapshot)?;
                let new: Value = serde_json::from_str(updated["detailText"].as_str().ok_or(InvalidFinalSnapshot)?).map_err(|_| InvalidFinalSnapshot)?;
                let detail = retained_tool_envelope(old, new)?;
                updated["text"] = prior["text"].clone();
                updated["detailText"] = json!(detail.to_string());
            }
            merged.push(updated);
            shared += 1;
            old_at = at + 1;
        } else {
            // Never deduplicate differing provider message identities by text.
            // Refuse an apparent duplicate rather than displaying it twice.
            if matches!(item["kind"].as_str(), Some("userMessage" | "agentMessage"))
                && previous_messages.contains(&(item["kind"].as_str().unwrap(), item["text"].as_str().unwrap_or("")))
            { return Err(InvalidFinalSnapshot); }
            pending.push(item.clone());
        }
    }
    merged.extend(previous[old_at..].iter().cloned());
    merged.append(&mut pending);
    for (order, item) in merged.iter_mut().enumerate() {
        item["transcriptOrder"] = json!(order);
    }
    next["items"] = json!(merged);
    Ok((previous.len() - shared, shared))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn final_union_updates_shared_items_and_preserves_output_and_order() {
        let old = turn(&json!({"id":"t","status":"inProgress","items":[
            {"id":"u","type":"userMessage","text":"input"},
            {"id":"tool","type":"commandExecution","status":"completed","aggregatedOutput":"output"},
            {"id":"a","type":"agentMessage","text":"partial"}
        ]}));
        let mut next = final_turn(&json!({"id":"t","status":"completed","items":[
            {"id":"u","type":"userMessage","text":"input"},
            {"id":"new","type":"reasoning","text":"new between shared anchors"},
            {"id":"a","type":"agentMessage","text":"final"}
        ]})).unwrap();
        assert_eq!(retain_final_items(&old, &mut next).unwrap(), (1,2));
        assert_eq!(next["items"].as_array().unwrap().iter().map(|i|i["id"].as_str().unwrap()).collect::<Vec<_>>(),vec!["u","tool","new","a"]);
        assert_eq!(next["items"][1]["detailText"],"output");
        assert_eq!(next["items"][3]["text"],"final");
        let before=next.clone();
        let mut repeated=next.clone();
        retain_final_items(&before,&mut repeated).unwrap();
        assert_eq!(before,repeated);
        let mut sparse_tool=final_turn(&json!({"id":"t","status":"completed","items":[{"id":"tool","type":"commandExecution","status":"completed"}]})).unwrap();
        retain_final_items(&before,&mut sparse_tool).unwrap();
        assert_eq!(sparse_tool["items"][1]["detailText"],"output");
    }
    #[test]
    fn final_identity_and_outcome_conflicts_do_not_modify_candidate() {
        let old=turn(&json!({"id":"t","status":"completed","items":[{"id":"tool","type":"commandExecution","status":"completed","aggregatedOutput":"output"}]}));
        for raw in [
            json!({"id":"other","status":"completed","items":[]}),
            json!({"id":"t","status":"failed","items":[]}),
            json!({"id":"t","status":"completed","error":{"message":"different"},"items":[]}),
            json!({"id":"t","status":"completed","items":[{"id":"tool","type":"commandExecution","status":"completed","aggregatedOutput":"changed"}]}),
            json!({"id":"t","status":"completed","items":[{"id":"tool","type":"commandExecution","status":"inProgress"}]}),
        ] {
            let mut candidate=final_turn(&raw).unwrap();let before=candidate.clone();
            assert!(retain_final_items(&old,&mut candidate).is_err());assert_eq!(candidate,before);
        }
        for id in [Value::Null,json!(3),json!(""),json!("x".repeat(257))] {
            assert!(final_turn(&json!({"id":"t","status":"completed","items":[{"id":id,"type":"agentMessage"}]})).is_err());
        }
        assert!(final_turn(&json!({"id":"t","status":"completed","items":[{"type":"agentMessage"}]})).is_err());
    }
    #[test]
    fn terminal_invocation_metadata_is_immutable_but_named_results_extend() {
        let old=json!({"id":"tool","type":"mcpToolCall","tool":"fixed","server":"fixture","status":"completed","arguments":{"input":"sealed"},"result":{"content":[{"type":"text","text":"one"}]}});
        let mut next=old.clone();next["result"]["content"].as_array_mut().unwrap().push(json!({"type":"text","text":"two"}));
        let merged=retained_tool_envelope(old.clone(),next).unwrap();
        assert_eq!(merged["result"]["content"].as_array().unwrap().len(),2);
        for field in ["type","tool","server","arguments"] {
            let mut changed=old.clone();
            changed[field]=if field=="arguments"{json!({"input":"sealed-extra"})}else{json!(format!("{}-extra",old[field].as_str().unwrap()))};
            assert!(retained_tool_envelope(old.clone(),changed).is_err());
        }
        let sparse=json!({"id":"tool","type":"mcpToolCall","tool":"","status":"completed","result":null});
        assert_eq!(retained_tool_envelope(old.clone(),sparse).unwrap(),old);
    }
    #[test]
    fn invalid_turn_identity_is_preserved_for_admission_rejection() {
        for raw in [
            json!({}),
            json!({"id":123}),
            json!({"id":{"secret":"never echo"}}),
        ] {
            assert!(!turn(&raw)["id"].is_string());
        }
    }
    #[test]
    fn plan_steps_use_browser_statuses() {
        let mapped = item(
            &json!({"type":"plan","plan":[
                {"step":"first","status":"completed"},
                {"step":"current","status":"in_progress"},
                {"step":"next","status":"pending"}
            ]}),
            "t",
            0,
        );
        assert_eq!(mapped["steps"][1]["status"], "inProgress");
        assert_eq!(mapped["steps"][0]["status"], "completed");
        assert_eq!(mapped["steps"][2]["status"], "pending");
    }
    #[test]
    fn utf8_preview_and_command_mapping() {
        assert_eq!(preview("é文x", 4), "é");
        let mapped = item(
            &json!({"id":"c","type":"commandExecution","command":"echo ok","aggregatedOutput":"ok"}),
            "t",
            0,
        );
        assert_eq!(mapped["kind"], "commandExecution");
        assert_eq!(mapped["detailText"], "ok");
    }
}
