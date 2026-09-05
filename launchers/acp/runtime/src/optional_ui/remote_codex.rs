use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::broadcast;
use treer_protocol::{AgentStatus, AgentTranscriptEntry};
use uuid::Uuid;

use super::{abort, submit_prompt, ApiError, AppState, PromptBody};
use crate::transcript::group_transcript_turns;
use crate::types::ModelOption;

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

pub fn turns_from_entries(
    entries: &[AgentTranscriptEntry],
    busy: bool,
    model: Option<&str>,
    reasoning_effort: Option<&str>,
    turn_usages: &std::collections::HashMap<String, Value>,
) -> Vec<Value> {
    let groups = group_transcript_turns(entries);
    let last_index = groups.len().saturating_sub(1);
    groups
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
            let status = if failed {
                "failed"
            } else if busy && index == last_index {
                "running"
            } else {
                "completed"
            };
            let turn_id = group
                .first()
                .map(|entry| entry.id.trim_end_matches(":user").to_string())
                .unwrap_or_else(|| "turn".into());
            let raw_usage = turn_usages.get(&turn_id);
            let turn_model = raw_usage
                .and_then(|usage| usage.get("model").and_then(Value::as_str))
                .or(model);
            let turn_effort = raw_usage
                .and_then(|usage| usage.get("reasoningEffort").and_then(Value::as_str))
                .or(reasoning_effort);
            let token_usage = raw_usage.and_then(crate::usage::normalize_usage);
            let price = token_usage.as_ref().and_then(|usage| {
                crate::usage::estimate_price(
                    usage,
                    turn_model,
                    raw_usage.and_then(|value| value.get("pricingTierKey").and_then(Value::as_str)),
                )
            });
            json!({
                "id": group.first().map(|entry| entry.id.as_str()).unwrap_or("turn"),
                "startedAt": if started.is_empty() { Value::Null } else { json!(started) },
                "status": status,
                "error": Value::Null,
                "items": group.iter().map(history_item).collect::<Vec<_>>(),
                "turnNumber": index + 1,
                "model": turn_model,
                "reasoningEffort": turn_effort,
                "tokenUsage": token_usage.as_ref().and_then(crate::usage::public_usage),
                "priceEstimate": price,
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
    pub turn_usages: std::collections::HashMap<String, Value>,
}

pub fn state_payload(input: SurfaceInput<'_>) -> Value {
    let now = crate::types::now_rfc3339();
    let busy = input.status == AgentStatus::Working;
    let turns = turns_from_entries(
        input.entries,
        busy,
        input.model.as_deref(),
        input.reasoning_effort.as_deref(),
        &input.turn_usages,
    );
    let title = thread_title(input.entries, input.harness_label);
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

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/state", get(state))
        .route("/api/prompt", post(prompt))
        .route("/api/interrupt", post(interrupt))
        .route("/api/settings", post(settings))
        .route("/ws", get(ws))
}

async fn state_value(state: &AppState) -> Value {
    let entries = state.journal.entries().unwrap_or_default();
    let (status, error) = {
        let current = state.status.lock().await;
        (current.status, current.error.clone())
    };
    let session_id = state
        .session_key
        .split_once("::")
        .map(|(_, rest)| rest)
        .unwrap_or(&state.session_key);
    let projection = state.runtime.session_projection(&state.session_key).await;
    state_payload(SurfaceInput {
        agent_id: &state.agent_id,
        harness_id: &state.harness_id,
        harness_label: &state.harness_label,
        cwd: &state.cwd.to_string_lossy(),
        session_id,
        entries: &entries,
        status,
        error: error.as_deref(),
        ready: true,
        model: projection.as_ref().and_then(|item| item.model.clone()),
        reasoning_effort: projection
            .as_ref()
            .and_then(|item| item.reasoning_effort.clone()),
        model_options: projection
            .as_ref()
            .map(|item| item.models.clone())
            .unwrap_or_default(),
        turn_usages: state.journal.turn_usages().unwrap_or_default(),
    })
}

pub(super) async fn emit_state(state: &AppState) {
    let _ = state.ui_events.send(state_value(state).await);
}

async fn state(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(state_value(&state).await)
}

#[derive(Debug, Deserialize)]
struct PromptRequest {
    prompt: Option<String>,
    text: Option<String>,
}

async fn prompt(
    State(state): State<Arc<AppState>>,
    Json(body): Json<PromptRequest>,
) -> Result<Json<Value>, ApiError> {
    let text = body
        .prompt
        .as_deref()
        .or(body.text.as_deref())
        .unwrap_or("")
        .trim()
        .to_string();
    if text.is_empty() {
        return Err(ApiError::bad("prompt is required"));
    }
    let _ = submit_prompt(
        State(state.clone()),
        Json(PromptBody {
            operation_id: Some(format!("ui_{}", Uuid::new_v4().simple())),
            text: Some(text),
        }),
    )
    .await?;
    let payload = state_value(&state).await;
    let _ = state.ui_events.send(payload.clone());
    Ok(Json(payload))
}

async fn interrupt(State(state): State<Arc<AppState>>) -> Json<Value> {
    let _ = abort(State(state.clone())).await;
    Json(state_value(&state).await)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SettingsRequest {
    model: Option<String>,
    reasoning_effort: Option<String>,
}

async fn settings(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SettingsRequest>,
) -> Result<Json<Value>, ApiError> {
    state
        .runtime
        .update_settings(
            &state.session_key,
            body.model.as_deref(),
            body.reasoning_effort.as_deref(),
        )
        .await
        .map_err(ApiError::from)?;
    let payload = state_value(&state).await;
    let _ = state.ui_events.send(payload.clone());
    Ok(Json(payload))
}

async fn ws(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| ws_session(socket, state))
}

async fn ws_session(mut socket: WebSocket, state: Arc<AppState>) {
    let mut rx = state.ui_events.subscribe();
    if socket
        .send(Message::Text(state_value(&state).await.to_string().into()))
        .await
        .is_err()
    {
        return;
    }
    loop {
        tokio::select! {
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Ping(payload))) => {
                        if socket.send(Message::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
            event = rx.recv() => {
                match event {
                    Ok(payload) => {
                        if socket.send(Message::Text(payload.to_string().into())).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        if socket
                            .send(Message::Text(state_value(&state).await.to_string().into()))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
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
            turn_usages: Default::default(),
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
            turn_usages: Default::default(),
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
        assert_eq!(turns[0]["status"], "completed");
    }

    #[test]
    fn last_turn_is_running_while_the_agent_is_working() {
        let entries = [entry("u1", "message", Some("user"), "hello")];
        let payload = state_payload(SurfaceInput {
            agent_id: "ag_1",
            harness_id: "grok",
            harness_label: "Grok",
            cwd: ".",
            session_id: "sess_1",
            entries: &entries,
            status: AgentStatus::Working,
            error: None,
            ready: true,
            model: None,
            reasoning_effort: None,
            model_options: Vec::new(),
            turn_usages: Default::default(),
        });
        assert_eq!(payload["detail"]["thread"]["status"], "running");
        assert_eq!(payload["detail"]["turns"][0]["status"], "running");
        assert_eq!(payload["detail"]["thread"]["activeTurnId"], "u1");
    }

    #[test]
    fn projects_turn_token_usage_and_price_estimate() {
        let entries = [
            entry("op-1:user", "message", Some("user"), "hi"),
            entry("op-1:assistant:1", "message", Some("assistant"), "hello"),
        ];
        let mut turn_usages = std::collections::HashMap::new();
        turn_usages.insert(
            "op-1".into(),
            json!({"total":{"inputTokens":1000,"outputTokens":20},"last":{"inputTokens":1000,"outputTokens":20},"model":"gpt-5.2"}),
        );
        let payload = state_payload(SurfaceInput {
            agent_id: "ag_1",
            harness_id: "codex",
            harness_label: "Codex",
            cwd: ".",
            session_id: "sess_1",
            entries: &entries,
            status: AgentStatus::Idle,
            error: None,
            ready: true,
            model: Some("gpt-5.2".into()),
            reasoning_effort: None,
            model_options: Vec::new(),
            turn_usages,
        });
        let turn = &payload["detail"]["turns"][0];
        assert_eq!(turn["tokenUsage"]["total"]["inputTokens"], 1000);
        assert_eq!(turn["tokenUsage"]["total"]["outputTokens"], 20);
        assert!(turn["priceEstimate"]["totalUsd"].as_f64().unwrap() > 0.0);
        assert_eq!(turn["model"], "gpt-5.2");
    }
}
