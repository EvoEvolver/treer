use serde_json::{json, Value};

use super::grok;
use crate::types::{ModelOption, ReasoningEffortOption};

#[derive(Debug, Clone)]
pub struct HarnessProjection {
    pub state: Value,
    pub models: Vec<ModelOption>,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
}

#[derive(Debug, Clone)]
pub enum SessionSettingOp {
    SetConfig { config_id: String, value: String },
    SetModel { model_id: String },
    SetMode { mode_id: String },
    LoadWithMeta { meta: Value },
}

/// Per-harness translator over a shared ACP client.
pub trait HarnessAdapter: Send + Sync {
    fn id(&self) -> &'static str;
    fn initialize_client_meta(&self) -> Value {
        json!({})
    }
    fn prompt_preamble(&self) -> Option<&'static str> {
        None
    }
    fn model_list_method(&self) -> Option<&'static str> {
        None
    }
    fn project_model_list(&self, _response: &Value) -> Option<Vec<ModelOption>> {
        None
    }
    fn fs_read_text_file(&self) -> bool {
        true
    }
    fn fs_write_text_file(&self) -> bool {
        true
    }
    fn session_new_meta(&self, _reasoning_effort: Option<&str>) -> Value {
        json!({})
    }
    fn project_session(&self, _response: &Value) -> Option<HarnessProjection> {
        None
    }
    fn apply_model(&self, _model: &str, _state: &Value) -> Option<SessionSettingOp> {
        None
    }
    fn apply_reasoning(&self, _effort: &str, _state: &Value) -> Option<SessionSettingOp> {
        None
    }
}

pub struct StandardAdapter;

impl HarnessAdapter for StandardAdapter {
    fn id(&self) -> &'static str {
        "standard"
    }
}

pub struct CodexAdapter;

impl HarnessAdapter for CodexAdapter {
    fn id(&self) -> &'static str {
        "codex"
    }
}

pub struct ClaudeAdapter;

impl HarnessAdapter for ClaudeAdapter {
    fn id(&self) -> &'static str {
        "claude"
    }
}

pub struct CursorAdapter;

impl HarnessAdapter for CursorAdapter {
    fn id(&self) -> &'static str {
        "cursor"
    }
    fn initialize_client_meta(&self) -> Value {
        json!({ "parameterizedModelPicker": true })
    }
    fn prompt_preamble(&self) -> Option<&'static str> {
        Some(
            "Cursor ACP client constraint: do not launch background subagents. If you delegate \
             work, wait for every subagent result in the current turn and deliver the complete \
             requested answer before ending the turn.",
        )
    }
    fn model_list_method(&self) -> Option<&'static str> {
        Some("cursor/list_available_models")
    }
    fn project_model_list(&self, response: &Value) -> Option<Vec<ModelOption>> {
        let models = response.get("models")?.as_array()?;
        let projected: Vec<_> = models
            .iter()
            .enumerate()
            .filter_map(|(index, model)| cursor_model(model, index))
            .collect();
        (!projected.is_empty()).then_some(projected)
    }
}

pub struct GrokAdapter;

impl HarnessAdapter for GrokAdapter {
    fn id(&self) -> &'static str {
        "grok"
    }
    fn fs_read_text_file(&self) -> bool {
        false
    }
    fn session_new_meta(&self, reasoning_effort: Option<&str>) -> Value {
        match grok::normalize_acp_effort(reasoning_effort) {
            Some(effort) => json!({ "reasoningEffort": effort }),
            None => json!({}),
        }
    }
    fn project_session(&self, response: &Value) -> Option<HarnessProjection> {
        grok::project_session(response)
    }
    fn apply_model(&self, model: &str, _state: &Value) -> Option<SessionSettingOp> {
        Some(grok::apply_model(model))
    }
    fn apply_reasoning(&self, effort: &str, state: &Value) -> Option<SessionSettingOp> {
        grok::apply_reasoning(effort, state)
    }
}

pub struct DeepSeekAdapter;

impl HarnessAdapter for DeepSeekAdapter {
    fn id(&self) -> &'static str {
        "deepseek"
    }
}

pub fn adapter_for(agent_id: &str) -> Box<dyn HarnessAdapter> {
    match agent_id {
        "codex" => Box::new(CodexAdapter),
        "claude" => Box::new(ClaudeAdapter),
        "cursor" => Box::new(CursorAdapter),
        "grok" => Box::new(GrokAdapter),
        "deepseek" => Box::new(DeepSeekAdapter),
        _ => Box::new(StandardAdapter),
    }
}

fn cursor_model(model: &Value, index: usize) -> Option<ModelOption> {
    let value = model.get("value").and_then(Value::as_str)?;
    let config_options = model
        .get("configOptions")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let reasoning = config_options.iter().find(|option| {
        option.get("category").and_then(Value::as_str) == Some("thought_level")
            && !select_options(option).is_empty()
    });
    let efforts: Vec<_> = reasoning
        .into_iter()
        .flat_map(select_options)
        .filter_map(|entry| {
            let raw = entry.get("value").and_then(Value::as_str)?;
            Some(ReasoningEffortOption {
                reasoning_effort: grok::normalize_acp_effort(Some(raw))?,
                description: entry
                    .get("description")
                    .or_else(|| entry.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            })
        })
        .collect();
    let default_reasoning_effort = reasoning
        .and_then(|option| option.get("currentValue"))
        .and_then(Value::as_str)
        .and_then(|value| grok::normalize_acp_effort(Some(value)));
    Some(ModelOption {
        id: value.to_string(),
        model: value.to_string(),
        display_name: model
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(value)
            .to_string(),
        description: model
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        is_default: index == 0,
        hidden: false,
        supported_reasoning_efforts: efforts,
        default_reasoning_effort,
    })
}

fn select_options(option: &Value) -> Vec<&Value> {
    option
        .get("options")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|entry| {
            if entry.get("options").is_some() {
                select_options(entry)
            } else {
                vec![entry]
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn grok_disables_fs_read_and_projects_session_models() {
        let adapter = GrokAdapter;
        assert!(!adapter.fs_read_text_file());
        assert_eq!(
            adapter.session_new_meta(Some("high")),
            json!({ "reasoningEffort": "high" })
        );
        let projected = adapter
            .project_session(&json!({
                "models": {
                    "currentModelId": "grok-4.6",
                    "availableModels": [{
                        "modelId": "grok-4.6",
                        "_meta": {
                            "reasoningEffort": "high",
                            "reasoningEfforts": [
                                { "value": "low" },
                                { "value": "high" }
                            ]
                        }
                    }]
                }
            }))
            .expect("grok projection");
        assert_eq!(projected.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(projected.models[0].supported_reasoning_efforts.len(), 2);
    }

    #[test]
    fn cursor_opts_into_and_projects_the_parameterized_model_picker() {
        let adapter = CursorAdapter;
        assert_eq!(
            adapter.initialize_client_meta(),
            json!({ "parameterizedModelPicker": true })
        );
        let response = json!({
            "models": [
                {
                    "value": "cursor-fast",
                    "name": "Cursor Fast",
                    "configOptions": [
                        {
                            "id": "thought-level",
                            "category": "thought_level",
                            "type": "select",
                            "currentValue": "high",
                            "options": [
                                { "value": "low", "name": "Low" },
                                { "value": "high", "name": "High" }
                            ]
                        },
                        { "id": "fast-mode", "type": "boolean", "currentValue": false }
                    ]
                },
                { "value": "cursor-accurate", "name": "Cursor Accurate" }
            ]
        });
        let models = adapter.project_model_list(&response).unwrap();
        assert_eq!(models.len(), 2);
        assert!(models[0].is_default);
        assert_eq!(models[0].model, "cursor-fast");
        assert_eq!(models[0].default_reasoning_effort.as_deref(), Some("high"));
        assert_eq!(
            models[0]
                .supported_reasoning_efforts
                .iter()
                .map(|effort| effort.reasoning_effort.as_str())
                .collect::<Vec<_>>(),
            vec!["low", "high"]
        );
        assert!(adapter
            .prompt_preamble()
            .unwrap()
            .contains("background subagents"));
    }
}
