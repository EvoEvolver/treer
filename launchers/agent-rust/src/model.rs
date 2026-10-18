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
    json!({"id":id,"status":raw["status"].as_str().unwrap_or("completed"),"startedAt":null,"error":raw["error"]["message"],"items":raw["items"].as_array().unwrap_or(&vec![]).iter().enumerate().map(|(n,i)|item(i,id,n)).collect::<Vec<_>>()})
}

#[cfg(test)]
mod tests {
    use super::*;
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
