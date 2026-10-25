//! First native Claude goal slice: explicit discovery, local-command ACK and
//! exact-condition completion. No private history, budget, interrupt or replay.
use serde_json::{json, Value};

pub fn supported(initialize: &Value) -> bool {
    initialize["commands"].as_array().is_some_and(|commands| {
        commands.len() <= 256 && commands.iter().any(|command| command["name"] == "goal")
    })
}

pub fn acknowledgement(event: &Value, input: &str) -> Option<Value> {
    if event["isSidechain"] == true || !event["parent_tool_use_id"].is_null() {
        return None;
    }
    let args = input.strip_prefix("/goal ")?;
    let command = event
        .get("commandRun")
        .or_else(|| event.get("local_command_run"))?;
    if command["command"] != "goal" || command["args"].as_str()?.trim() != args {
        return None;
    }
    let output = if event["type"] == "system" && event["subtype"] == "local_command" {
        event["content"]
            .as_str()?
            .strip_prefix("<local-command-stdout>")?
            .strip_suffix("</local-command-stdout>")?
            .to_owned()
    } else if event["type"] == "assistant"
        && event["message"]["model"] == "<synthetic>"
        && event["local_command_source"].is_string()
    {
        let blocks = event["message"]["content"].as_array()?;
        if blocks.iter().any(|block| block["type"] != "text") {
            return None;
        }
        blocks
            .iter()
            .filter_map(|block| block["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        return None;
    };
    let output = output.trim();
    if args == "clear" {
        if !["Goal cleared", "No goal set"].iter().any(|prefix| {
            output == *prefix
                || output
                    .strip_prefix(prefix)
                    .is_some_and(|tail| tail.starts_with('.') || tail.starts_with(':'))
        }) {
            return None;
        }
        Some(json!({"args":args,"goal":null}))
    } else if output.strip_prefix("Goal set: ") == Some(args) {
        Some(json!({"args":args,"goal":{"objective":args,"status":"active"}}))
    } else {
        None
    }
}

pub fn completions(event: &Value) -> Vec<Value> {
    if event["isSidechain"] == true || !event["parent_tool_use_id"].is_null() {
        return vec![];
    }
    if event["type"] == "system" && event["subtype"] == "stop_hook_summary" {
        // Live SDK stdout is snake_case; native persisted envelopes are not read.
        if event["has_output"] != false
            || event["prevented_continuation"] != false
            || !event["hook_errors"].as_array().is_some_and(Vec::is_empty)
        {
            return vec![];
        }
        let Some(hooks) = event["hook_infos"]
            .as_array()
            .filter(|hooks| hooks.len() <= 128)
        else {
            return vec![];
        };
        return hooks
            .iter()
            .filter(|hook| hook["result"]["ok"] != false && hook["result"]["impossible"] != true)
            .filter_map(|hook| {
                hook["prompt_text"]
                    .as_str()
                    .filter(|text| !text.is_empty() && text.len() <= 65_000)
            })
            .map(|condition| json!({"condition":condition,"status":"done"}))
            .collect();
    }
    completion(event).into_iter().collect()
}
fn completion(event: &Value) -> Option<Value> {
    if event["type"] != "attachment" {
        return None;
    }
    let value = &event["attachment"];
    let condition = value["condition"]
        .as_str()
        .filter(|text| !text.is_empty() && text.len() <= 65_000)?;
    if value["type"] != "goal_status" {
        return None;
    }
    let status = match value["status"].as_str() {
        Some("met" | "achieved") => "done",
        Some("failed" | "impossible") => "blocked",
        _ if value["met"] == true && value["sentinel"] != true => "done",
        _ => return None,
    };
    Some(json!({"condition":condition,"status":status}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_envelope_and_exact_args_are_required() {
        let event = json!({"type":"assistant","message":{"model":"<synthetic>","content":[{"type":"text","text":"Goal set: exact objective"}]},"local_command_source":"native","local_command_run":{"command":"goal","args":"exact objective"}});
        assert!(acknowledgement(&event, "/goal exact objective").is_some());
        assert!(acknowledgement(&event, "/goal objective").is_none());
        let mut wrong = event.clone();
        wrong["message"]["model"] = json!("real-model");
        assert!(acknowledgement(&wrong, "/goal exact objective").is_none());
        wrong = event;
        wrong["parent_tool_use_id"] = json!("child");
        assert!(acknowledgement(&wrong, "/goal exact objective").is_none());
        assert!(!supported(&json!({})));
        assert!(supported(&json!({"commands":[{"name":"goal"}]})));
    }
    #[test]
    fn only_explicit_native_hook_completion_has_authority() {
        let event = json!({"type":"system","subtype":"stop_hook_summary","has_output":false,"prevented_continuation":false,"hook_errors":[],"hook_infos":[{"prompt_text":"exact condition"}]});
        assert_eq!(
            completions(&event),
            vec![json!({"condition":"exact condition","status":"done"})]
        );
        let mut wrong = event.clone();
        wrong["has_output"] = json!(true);
        assert!(completions(&wrong).is_empty());
        wrong = event;
        wrong["hook_errors"] = json!(["error"]);
        assert!(completions(&wrong).is_empty());
        wrong["hook_errors"] = json!([]);
        wrong["hook_infos"][0]["result"] = json!({"ok":false,"impossible":true});
        assert!(completions(&wrong).is_empty());
    }
}
