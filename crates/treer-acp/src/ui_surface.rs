use serde_json::{json, Value};
use treer_protocol::{AgentStatus, AgentTranscriptEntry};

use crate::transcript::group_transcript_turns;
use crate::types::ModelOption;

pub fn harness_label(id: &str) -> String {
    match id {
        "grok" => "Grok".into(),
        "cursor" => "Cursor".into(),
        "codex" => "Codex".into(),
        "claude" => "Claude".into(),
        "opencode" => "OpenCode".into(),
        "fake" => "Fake ACP".into(),
        other => other.to_string(),
    }
}

pub fn entry_text(entry: &AgentTranscriptEntry) -> String {
    match &entry.content {
        Value::String(text) => text.clone(),
        Value::Object(object) => object
            .get("text")
            .and_then(Value::as_str)
            .or_else(|| object.get("preview_text").and_then(Value::as_str))
            .unwrap_or("")
            .to_string(),
        other => other.to_string(),
    }
}

pub fn history_kind(entry: &AgentTranscriptEntry) -> &'static str {
    match (entry.kind.as_str(), entry.role.as_deref()) {
        ("message", Some("user")) => "userMessage",
        ("message", Some("assistant")) => "agentMessage",
        ("reasoning", _) => "reasoning",
        ("commandExecution", _) => "commandExecution",
        ("fileRead", _) => "fileRead",
        ("fileChange", _) => "fileChange",
        ("webSearch", _) => "webSearch",
        ("toolCall" | "agentToolCall", _) => "toolCall",
        _ => "other",
    }
}

pub fn history_item(entry: &AgentTranscriptEntry) -> Value {
    let text = entry_text(entry);
    let mut item = json!({
        "id": entry.id,
        "kind": history_kind(entry),
        "text": text,
    });
    if let Some(created_at) = &entry.created_at {
        item["createdAt"] = json!(created_at);
    }
    if let Value::Object(object) = &entry.content {
        if let Some(preview) = object.get("preview_text") {
            item["previewText"] = preview.clone();
        }
        if let Some(detail) = object.get("detail_text") {
            item["detailText"] = detail.clone();
        }
        if let Some(status) = object.get("status") {
            item["status"] = status.clone();
        }
    }
    item
}

pub fn turns_from_entries(entries: &[AgentTranscriptEntry]) -> Vec<Value> {
    group_transcript_turns(entries)
        .into_iter()
        .enumerate()
        .map(|(index, group)| {
            let started = group
                .iter()
                .find_map(|entry| entry.created_at.clone())
                .unwrap_or_default();
            let failed = group.iter().any(|entry| {
                matches!(entry.kind.as_str(), "error")
                    || entry.content.get("status").and_then(Value::as_str) == Some("failed")
            });
            json!({
                "id": group.first().map(|entry| entry.id.as_str()).unwrap_or("turn"),
                "startedAt": if started.is_empty() { Value::Null } else { json!(started) },
                "status": if failed { "failed" } else { "completed" },
                "error": Value::Null,
                "items": group.iter().map(history_item).collect::<Vec<_>>(),
                "turnNumber": index + 1,
            })
        })
        .collect()
}

pub fn thread_title(entries: &[AgentTranscriptEntry], fallback: &str) -> String {
    entries
        .iter()
        .find(|entry| entry.kind == "message" && entry.role.as_deref() == Some("user"))
        .map(entry_text)
        .filter(|text| !text.trim().is_empty())
        .map(|text| crate::types::truncate_title(&text))
        .unwrap_or_else(|| fallback.to_string())
}

pub struct SurfaceInput<'a> {
    pub agent_id: &'a str,
    pub harness_id: &'a str,
    pub harness_label: &'a str,
    pub cwd: &'a str,
    pub session_id: &'a str,
    pub entries: &'a [AgentTranscriptEntry],
    pub status: AgentStatus,
    pub error: Option<&'a str>,
    pub ready: bool,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub model_options: Vec<ModelOption>,
}

pub fn state_payload(input: SurfaceInput<'_>) -> Value {
    let now = crate::types::now_rfc3339();
    let turns = turns_from_entries(input.entries);
    let title = thread_title(input.entries, input.harness_label);
    let busy = input.status == AgentStatus::Working;
    let thread_status = if busy {
        "running"
    } else if input.status == AgentStatus::Blocked {
        "error"
    } else {
        "idle"
    };
    let created = input
        .entries
        .first()
        .and_then(|entry| entry.created_at.clone())
        .unwrap_or_else(|| now.clone());
    let updated = input
        .entries
        .last()
        .and_then(|entry| entry.created_at.clone())
        .unwrap_or_else(|| now.clone());
    let thread = json!({
        "id": input.session_id,
        "workspaceId": "local",
        "provider": input.harness_id,
        "providerSessionId": input.session_id,
        "source": "supervisor",
        "title": title,
        "model": input.model,
        "reasoningEffort": input.reasoning_effort,
        "fastMode": false,
        "collaborationMode": "default",
        "approvalMode": "yolo",
        "sandboxMode": "workspace-write",
        "status": thread_status,
        "summaryText": Value::Null,
        "lastError": input.error,
        "activeTurnId": if busy { turns.last().and_then(|turn| turn.get("id").cloned()) } else { None },
        "isLoaded": true,
        "isPinned": false,
        "createdAt": created,
        "updatedAt": updated,
        "lastTurnStartedAt": turns.last().and_then(|turn| turn.get("startedAt").cloned()),
        "lastTurnCompletedAt": if busy { Value::Null } else { json!(updated) },
    });
    json!({
        "type": "state",
        "ready": input.ready,
        "auth": {
            "harnessId": input.harness_id,
            "displayName": input.harness_label,
            "status": "authenticated",
            "methods": [],
            "error": Value::Null,
            "login": {
                "available": false,
                "status": "idle",
                "output": "",
                "urls": [],
                "deviceCode": Value::Null,
                "error": Value::Null,
            },
        },
        "cwd": input.cwd,
        "root": input.cwd,
        "agentId": input.agent_id,
        "status": {
            "state": if input.ready { "ready" } else { "starting" },
            "transport": "stdio",
            "lastStartedAt": now,
            "lastError": input.error,
            "restartCount": 0,
        },
        "modelOptions": input.model_options,
        "threads": [thread.clone()],
        "detail": {
            "thread": thread,
            "workspace": {
                "id": "local",
                "hostId": "local",
                "label": input.harness_label,
                "absPath": input.cwd,
                "isFavorite": false,
                "createdAt": created,
                "lastOpenedAt": updated,
            },
            "workspacePathStatus": "present",
            "totalTurnCount": turns.len(),
            "pendingRequests": [],
            "pendingSteers": [],
            "turns": turns,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, kind: &str, role: Option<&str>, text: &str) -> AgentTranscriptEntry {
        AgentTranscriptEntry {
            id: id.into(),
            kind: kind.into(),
            role: role.map(str::to_string),
            content: json!(text),
            created_at: Some("2026-09-04T20:35:27.848Z".into()),
        }
    }

    #[test]
    fn state_payload_always_has_detail_so_the_thread_ui_can_leave_loading() {
        let payload = state_payload(SurfaceInput {
            agent_id: "ag_1",
            harness_id: "grok",
            harness_label: "Grok",
            cwd: "/workspace/treer",
            session_id: "sess_1",
            entries: &[],
            status: AgentStatus::Idle,
            error: None,
            ready: true,
            model: None,
            reasoning_effort: None,
            model_options: Vec::new(),
        });
        assert_eq!(payload["ready"], true);
        assert_eq!(payload["detail"]["thread"]["id"], "sess_1");
        assert_eq!(payload["detail"]["thread"]["provider"], "grok");
        assert_eq!(payload["detail"]["turns"].as_array().unwrap().len(), 0);
        assert_eq!(payload["auth"]["displayName"], "Grok");
        assert_eq!(payload["modelOptions"].as_array().unwrap().len(), 0);
        assert_eq!(payload["detail"]["thread"]["model"], Value::Null);
    }

    #[test]
    fn groups_user_and_assistant_into_turns() {
        let entries = [
            entry("u1", "message", Some("user"), "Reply with pong"),
            entry("a1", "message", Some("assistant"), "pong"),
        ];
        let payload = state_payload(SurfaceInput {
            agent_id: "ag_1",
            harness_id: "grok",
            harness_label: "Grok",
            cwd: ".",
            session_id: "sess_1",
            entries: &entries,
            status: AgentStatus::Idle,
            error: None,
            ready: true,
            model: Some("grok-4.6".into()),
            reasoning_effort: Some("high".into()),
            model_options: vec![ModelOption {
                id: "grok-4.6".into(),
                model: "grok-4.6".into(),
                display_name: "Grok 4.6".into(),
                description: String::new(),
                is_default: true,
                hidden: false,
                supported_reasoning_efforts: Vec::new(),
                default_reasoning_effort: Some("high".into()),
            }],
        });
        let turns = payload["detail"]["turns"].as_array().unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0]["items"][0]["kind"], "userMessage");
        assert_eq!(turns[0]["items"][1]["kind"], "agentMessage");
        assert_eq!(turns[0]["items"][1]["text"], "pong");
        assert_eq!(payload["detail"]["thread"]["title"], "Reply with pong");
        assert_eq!(payload["detail"]["thread"]["model"], "grok-4.6");
        assert_eq!(payload["detail"]["thread"]["reasoningEffort"], "high");
        assert_eq!(payload["modelOptions"][0]["displayName"], "Grok 4.6");
        assert_eq!(payload["modelOptions"][0]["isDefault"], true);
    }
}
