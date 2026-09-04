use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use tokio::sync::{broadcast, mpsc, Mutex};

use super::adapter::{adapter_for, HarnessAdapter, HarnessProjection, SessionSettingOp};
use super::capabilities::{negotiate, NegotiatedCaps};
use super::catalog::{builtin_agents, classify_availability, AcpAgentDef};
use super::mapper::TurnMapper;
use super::modes::{parse_permission_choices, select_permission_option};
use super::prompt::build_prompt_blocks;
use super::rpc::AcpProcess;
use super::terminal::AgentTerminals;
use crate::cancel::Cancel;
use crate::files::{write_file_with_scope, WriteScope};
use crate::import_id::session_ids_match;
use crate::types::HistoryItem;

struct ActiveTurn {
    turn_id: String,
}

enum TurnOutcome {
    Completed,
    Interrupted,
    Failed(anyhow::Error),
}

struct LiveSession {
    process: Arc<AcpProcess>,
    session_id: String,
    cwd: PathBuf,
    negotiated: NegotiatedCaps,
    adapter_id: String,
    projection: Option<HarnessProjection>,
    active: Option<ActiveTurn>,
}

struct Inner {
    sessions: Mutex<HashMap<String, LiveSession>>,
    updates: broadcast::Sender<Value>,
    terminals: AgentTerminals,
}

pub struct AcpRuntime {
    custom_command: Option<String>,
    startup_timeout: Duration,
    inner: Arc<Inner>,
}

impl AcpRuntime {
    pub fn catalog(custom: Option<String>, timeout_ms: u64) -> Self {
        let (updates, _) = broadcast::channel(2048);
        Self {
            custom_command: custom,
            startup_timeout: Duration::from_millis(timeout_ms),
            inner: Arc::new(Inner {
                sessions: Mutex::new(HashMap::new()),
                updates,
                terminals: AgentTerminals::default(),
            }),
        }
    }

    pub fn agent_def(&self, agent_id: Option<&str>) -> Result<AcpAgentDef> {
        let id = agent_id.unwrap_or("codex");
        builtin_agents(self.custom_command.as_deref())
            .into_iter()
            .find(|agent| agent.id == id)
            .ok_or_else(|| anyhow!("unknown ACP agent {id}"))
    }

    pub fn scoped_id(agent_id: &str, session_id: &str) -> String {
        format!("{agent_id}::{session_id}")
    }

    async fn spawn_session(
        &self,
        def: &AcpAgentDef,
        cwd: &str,
        load_id: Option<&str>,
    ) -> Result<(String, LiveSession)> {
        let availability = classify_availability(def);
        if availability != "ready" {
            bail!("{} is not available ({availability})", def.display_name);
        }
        let adapter = adapter_for(&def.id);
        let extra_env = extra_env_for(def);
        let server_command = agent_server_command(def);
        let (process, updates_rx, requests_rx) = tokio::time::timeout(
            self.startup_timeout,
            AcpProcess::spawn(&server_command, cwd, &extra_env),
        )
        .await
        .map_err(|_| anyhow!("ACP spawn timeout"))??;
        let process = Arc::new(process);
        let init = process
            .request(
                "initialize",
                json!({
                    "protocolVersion": 1,
                    "clientInfo": {
                        "name": "treer-acp",
                        "title": "Treer ACP",
                        "version": env!("CARGO_PKG_VERSION")
                    },
                    "clientCapabilities": {
                        "fs": {
                            "readTextFile": adapter.fs_read_text_file(),
                            "writeTextFile": adapter.fs_write_text_file()
                        },
                        "terminal": true,
                        "session": { "list": {} },
                        "_meta": adapter.initialize_client_meta()
                    }
                }),
            )
            .await?;
        let negotiated = negotiate(&init);
        spawn_mux(self.inner.clone(), process.clone(), updates_rx, requests_rx);
        let raw_session = if let Some(existing) = load_id {
            if negotiated.load_session {
                process
                    .request(
                        "session/load",
                        json!({ "sessionId": existing, "cwd": cwd, "mcpServers": [] }),
                    )
                    .await?
            } else if negotiated.resume {
                process
                    .request(
                        "session/resume",
                        json!({ "sessionId": existing, "cwd": cwd, "mcpServers": [] }),
                    )
                    .await?
            } else {
                bail!("ACP agent does not support session/load or session/resume");
            }
        } else {
            let extra_meta = adapter.session_new_meta(None);
            let mut meta = json!({ "yoloMode": true });
            if let Some(map) = extra_meta.as_object() {
                for (key, value) in map {
                    meta[key] = value.clone();
                }
            }
            process
                .request(
                    "session/new",
                    json!({ "cwd": cwd, "mcpServers": [], "_meta": meta }),
                )
                .await?
        };
        let session_id = raw_session
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or(load_id.unwrap_or(""))
            .to_string();
        if session_id.is_empty() {
            bail!("ACP session id missing");
        }
        let scoped = Self::scoped_id(&def.id, &session_id);
        let projection =
            load_session_projection(adapter.as_ref(), process.as_ref(), &raw_session).await;
        let live = LiveSession {
            process,
            session_id,
            cwd: PathBuf::from(cwd),
            negotiated,
            adapter_id: def.id.clone(),
            projection,
            active: None,
        };
        Ok((scoped, live))
    }

    pub async fn start_session(&self, def: &AcpAgentDef, cwd: &Path) -> Result<String> {
        let (scoped, live) = self
            .spawn_session(def, &cwd.to_string_lossy(), None)
            .await?;
        self.inner
            .sessions
            .lock()
            .await
            .insert(scoped.clone(), live);
        Ok(scoped)
    }

    pub async fn load_session(
        &self,
        def: &AcpAgentDef,
        cwd: &Path,
        session_id: &str,
    ) -> Result<String> {
        {
            let sessions = self.inner.sessions.lock().await;
            if let Some(existing) = sessions
                .keys()
                .find(|key| session_ids_match(key, session_id))
                .cloned()
            {
                return Ok(existing);
            }
        }
        let raw = session_id
            .split_once("::")
            .map(|(_, rest)| rest)
            .unwrap_or(session_id);
        let (scoped, live) = self
            .spawn_session(def, &cwd.to_string_lossy(), Some(raw))
            .await?;
        self.inner
            .sessions
            .lock()
            .await
            .insert(scoped.clone(), live);
        Ok(scoped)
    }

    pub async fn prompt(
        &self,
        session_key: &str,
        prompt: &str,
        turn_id: &str,
        cancel: Cancel,
    ) -> Result<Vec<HistoryItem>> {
        self.prompt_with_progress(session_key, prompt, turn_id, cancel, |_| {})
            .await
    }

    pub async fn prompt_with_progress<F>(
        &self,
        session_key: &str,
        prompt: &str,
        turn_id: &str,
        cancel: Cancel,
        mut on_progress: F,
    ) -> Result<Vec<HistoryItem>>
    where
        F: FnMut(&[HistoryItem]) + Send,
    {
        {
            let sessions = self.inner.sessions.lock().await;
            let live = sessions
                .get(session_key)
                .ok_or_else(|| anyhow!("ACP session is not running"))?;
            if live.active.is_some() {
                bail!("ACP session already has an active turn");
            }
        }
        let (process, session_id, cwd, image_capable, adapter_id) = {
            let sessions = self.inner.sessions.lock().await;
            let live = sessions
                .get(session_key)
                .ok_or_else(|| anyhow!("ACP session is not running"))?;
            (
                live.process.clone(),
                live.session_id.clone(),
                live.cwd.clone(),
                live.negotiated.image,
                live.adapter_id.clone(),
            )
        };
        let adapter = adapter_for(&adapter_id);
        let prompt = adapter
            .prompt_preamble()
            .map(|preamble| format!("{preamble}\n\n{prompt}"))
            .unwrap_or_else(|| prompt.to_string());
        let prompt_blocks = build_prompt_blocks(&prompt, &cwd, image_capable, &[])?;
        let mut updates = self.inner.updates.subscribe();
        {
            let mut sessions = self.inner.sessions.lock().await;
            let live = sessions
                .get_mut(session_key)
                .ok_or_else(|| anyhow!("ACP session is not running"))?;
            if live.active.is_some() {
                bail!("ACP session already has an active turn");
            }
            live.active = Some(ActiveTurn {
                turn_id: turn_id.to_string(),
            });
        }
        let prompt_rpc = process.request(
            "session/prompt",
            json!({
                "sessionId": session_id,
                "prompt": prompt_blocks
            }),
        );
        let mut mapper = TurnMapper::new(turn_id);
        tokio::pin!(prompt_rpc);
        let mut prompt_done = false;
        let outcome = loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    let _ = process.notify("session/cancel", json!({ "sessionId": session_id })).await;
                    break TurnOutcome::Interrupted;
                }
                result = &mut prompt_rpc, if !prompt_done => {
                    match result {
                        Ok(response) if response.get("stopReason").and_then(Value::as_str) == Some("cancelled") => {
                            cancel.cancel();
                            break TurnOutcome::Interrupted;
                        }
                        Ok(_) => prompt_done = true,
                        Err(err) => {
                            tracing::warn!(error = %err, session_id = %session_id, "ACP session/prompt failed");
                            break TurnOutcome::Failed(anyhow!("ACP session/prompt failed: {err}"));
                        }
                    }
                }
                recv = updates.recv() => {
                    match recv {
                        Ok(update) => {
                            if let Some(sid) = update.get("sessionId").and_then(Value::as_str) {
                                if sid != session_id {
                                    continue;
                                }
                            }
                            let _ = mapper.apply(&update);
                            on_progress(&mapper.snapshot());
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                            tracing::warn!(skipped, "ACP session/update receiver lagged");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            break if prompt_done {
                                TurnOutcome::Completed
                            } else {
                                TurnOutcome::Failed(anyhow!("ACP update channel closed during session/prompt"))
                            };
                        }
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(250)), if prompt_done => {
                    break TurnOutcome::Completed;
                }
                _ = tokio::time::sleep(Duration::from_millis(500)), if !prompt_done => {
                    match process.exited().await {
                        Ok(true) => {
                            break TurnOutcome::Failed(anyhow!("ACP process exited before session/prompt completed"));
                        }
                        Ok(false) => {}
                        Err(err) => {
                            break TurnOutcome::Failed(anyhow!("failed to inspect ACP process: {err}"));
                        }
                    }
                }
            }
        };
        if let Some(live) = self.inner.sessions.lock().await.get_mut(session_key) {
            if live
                .active
                .as_ref()
                .is_some_and(|active| active.turn_id == turn_id)
            {
                live.active = None;
            }
        }
        let status = match &outcome {
            TurnOutcome::Completed => "completed",
            TurnOutcome::Interrupted => "interrupted",
            TurnOutcome::Failed(_) => "failed",
        };
        let mut items = mapper.finish(!matches!(outcome, TurnOutcome::Completed));
        for item in &mut items {
            if item.status.as_deref() != Some("failed") {
                item.status = Some(status.into());
            }
        }
        match outcome {
            TurnOutcome::Failed(error) => Err(error),
            TurnOutcome::Completed | TurnOutcome::Interrupted => Ok(items),
        }
    }

    pub async fn interrupt(&self, session_key: &str) -> Result<()> {
        if let Some(live) = self.inner.sessions.lock().await.get(session_key) {
            live.process
                .notify("session/cancel", json!({ "sessionId": live.session_id }))
                .await?;
        }
        Ok(())
    }

    pub async fn session_projection(&self, session_key: &str) -> Option<HarnessProjection> {
        self.inner
            .sessions
            .lock()
            .await
            .get(session_key)
            .and_then(|live| live.projection.clone())
    }

    pub async fn update_settings(
        &self,
        session_key: &str,
        model: Option<&str>,
        reasoning_effort: Option<&str>,
    ) -> Result<Option<HarnessProjection>> {
        let (process, session_id, cwd, adapter_id, mut projection) = {
            let sessions = self.inner.sessions.lock().await;
            let live = sessions
                .get(session_key)
                .ok_or_else(|| anyhow!("ACP session is not running"))?;
            (
                live.process.clone(),
                live.session_id.clone(),
                live.cwd.clone(),
                live.adapter_id.clone(),
                live.projection.clone(),
            )
        };
        let adapter = adapter_for(&adapter_id);
        let state = projection
            .as_ref()
            .map(|item| item.state.clone())
            .unwrap_or_else(|| json!({}));
        if let Some(model) = model.map(str::trim).filter(|value| !value.is_empty()) {
            let current = projection.as_ref().and_then(|item| item.model.as_deref());
            if current != Some(model) {
                let op = adapter
                    .apply_model(model, &state)
                    .ok_or_else(|| anyhow!("this harness cannot change models"))?;
                let response = apply_setting_op(process.as_ref(), &session_id, &cwd, op).await?;
                patch_projection(
                    &mut projection,
                    adapter.as_ref(),
                    Some(model),
                    None,
                    &response,
                );
            }
        }
        let state = projection
            .as_ref()
            .map(|item| item.state.clone())
            .unwrap_or(state);
        if let Some(effort) = reasoning_effort
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            let current = projection
                .as_ref()
                .and_then(|item| item.reasoning_effort.as_deref());
            if current != Some(effort) {
                if let Some(op) = adapter.apply_reasoning(effort, &state) {
                    let response =
                        apply_setting_op(process.as_ref(), &session_id, &cwd, op).await?;
                    patch_projection(
                        &mut projection,
                        adapter.as_ref(),
                        None,
                        Some(effort),
                        &response,
                    );
                }
            }
        }
        if let Some(live) = self.inner.sessions.lock().await.get_mut(session_key) {
            live.projection = projection.clone();
        }
        Ok(projection)
    }

    pub fn session_loaded(&self, session_id: &str) -> bool {
        self.inner
            .sessions
            .try_lock()
            .ok()
            .map(|sessions| {
                sessions.iter().any(|(key, live)| {
                    session_ids_match(key, session_id)
                        || session_ids_match(&live.session_id, session_id)
                })
            })
            .unwrap_or(false)
    }

    pub async fn process_alive(&self, session_key: &str) -> bool {
        let sessions = self.inner.sessions.lock().await;
        let Some(live) = sessions.get(session_key) else {
            return false;
        };
        !live.process.exited().await.unwrap_or(true)
    }
}

async fn load_session_projection(
    adapter: &dyn HarnessAdapter,
    process: &AcpProcess,
    raw_session: &Value,
) -> Option<HarnessProjection> {
    let from_session = adapter.project_session(raw_session);
    let from_list = if let Some(method) = adapter.model_list_method() {
        match process.request(method, json!({})).await {
            Ok(response) => adapter.project_model_list(&response).and_then(|models| {
                let selected = models
                    .iter()
                    .find(|model| model.is_default)
                    .or(models.first())?;
                Some(HarnessProjection {
                    state: response,
                    model: Some(selected.model.clone()),
                    reasoning_effort: selected.default_reasoning_effort.clone(),
                    models,
                })
            }),
            Err(error) => {
                tracing::warn!(%error, "ACP model list failed");
                None
            }
        }
    } else {
        None
    };
    match (from_list, from_session) {
        (Some(list), Some(session)) => Some(HarnessProjection {
            state: if list.state.is_null() {
                session.state
            } else {
                list.state
            },
            models: if list.models.is_empty() {
                session.models
            } else {
                list.models
            },
            model: list.model.or(session.model),
            reasoning_effort: list.reasoning_effort.or(session.reasoning_effort),
        }),
        (Some(list), None) => Some(list),
        (None, Some(session)) => Some(session),
        (None, None) => None,
    }
}

async fn apply_setting_op(
    process: &AcpProcess,
    session_id: &str,
    cwd: &Path,
    op: SessionSettingOp,
) -> Result<Value> {
    match op {
        SessionSettingOp::SetConfig { config_id, value } => {
            process
                .request(
                    "session/set_config_option",
                    json!({
                        "sessionId": session_id,
                        "configId": config_id,
                        "value": value,
                    }),
                )
                .await
        }
        SessionSettingOp::SetModel { model_id } => {
            process
                .request(
                    "session/set_model",
                    json!({
                        "sessionId": session_id,
                        "modelId": model_id,
                    }),
                )
                .await
        }
        SessionSettingOp::SetMode { mode_id } => {
            process
                .request(
                    "session/set_mode",
                    json!({
                        "sessionId": session_id,
                        "modeId": mode_id,
                    }),
                )
                .await
        }
        SessionSettingOp::LoadWithMeta { meta } => {
            process
                .request(
                    "session/load",
                    json!({
                        "sessionId": session_id,
                        "cwd": cwd,
                        "mcpServers": [],
                        "_meta": meta,
                    }),
                )
                .await
        }
    }
}

fn patch_projection(
    projection: &mut Option<HarnessProjection>,
    adapter: &dyn HarnessAdapter,
    model: Option<&str>,
    reasoning_effort: Option<&str>,
    response: &Value,
) {
    if let Some(from_response) = adapter.project_session(response) {
        *projection = Some(from_response);
    }
    let Some(current) = projection.as_mut() else {
        if let Some(model) = model {
            *projection = Some(HarnessProjection {
                state: response.clone(),
                models: Vec::new(),
                model: Some(model.to_string()),
                reasoning_effort: reasoning_effort.map(str::to_string),
            });
        }
        return;
    };
    if let Some(model) = model {
        current.model = Some(model.to_string());
        if let Some(object) = current.state.as_object_mut() {
            object.insert("currentModelId".into(), json!(model));
        }
    }
    if let Some(effort) = reasoning_effort {
        current.reasoning_effort = Some(effort.to_string());
    }
}

fn spawn_mux(
    inner: Arc<Inner>,
    process: Arc<AcpProcess>,
    mut updates: mpsc::UnboundedReceiver<Value>,
    mut requests: mpsc::UnboundedReceiver<(i64, String, Value)>,
) {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                Some(update) = updates.recv() => {
                    let _ = inner.updates.send(update);
                }
                Some((req_id, method, params)) = requests.recv() => {
                    let inner = inner.clone();
                    let process = process.clone();
                    tokio::spawn(async move {
                        if let Err(err) = handle_agent_request(&inner, &process, req_id, &method, params).await {
                            tracing::warn!(error = %err, method, req_id, "ACP client request failed");
                            let _ = process.respond_error(req_id, &err.to_string()).await;
                        }
                    });
                }
                else => break,
            }
        }
    });
}

async fn handle_agent_request(
    inner: &Inner,
    process: &AcpProcess,
    req_id: i64,
    method: &str,
    params: Value,
) -> Result<()> {
    let raw_session = params
        .get("sessionId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let cwd = {
        let sessions = inner.sessions.lock().await;
        sessions
            .values()
            .find(|session| session.session_id == raw_session)
            .map(|session| session.cwd.clone())
            .unwrap_or_else(|| PathBuf::from("."))
    };
    match method {
        "session/request_permission" => {
            let choices = parse_permission_choices(&params);
            if let Some(option) = select_permission_option(&choices, true, None) {
                process
                    .respond(
                        req_id,
                        json!({ "outcome": { "outcome": "selected", "optionId": option } }),
                    )
                    .await?;
            } else {
                process
                    .respond(req_id, json!({ "outcome": { "outcome": "cancelled" } }))
                    .await?;
            }
        }
        "fs/read_text_file" => {
            let path = params.get("path").and_then(Value::as_str).unwrap_or("");
            let abs = if Path::new(path).is_absolute() {
                PathBuf::from(path)
            } else {
                cwd.join(path)
            };
            let content = match crate::files::assert_within(&cwd, &abs) {
                Ok(resolved) => tokio::fs::read_to_string(resolved)
                    .await
                    .unwrap_or_default(),
                Err(_) => String::new(),
            };
            process
                .respond(req_id, json!({ "content": content }))
                .await?;
        }
        "fs/write_text_file" => {
            let path = params.get("path").and_then(Value::as_str).unwrap_or("");
            let content = params.get("content").and_then(Value::as_str).unwrap_or("");
            match write_file_with_scope(&cwd, path, content, WriteScope::Workspace) {
                Ok(()) => process.respond(req_id, json!({})).await?,
                Err(err) => process.respond_error(req_id, &err.to_string()).await?,
            }
        }
        "terminal/create" => {
            let command = params
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or("sh");
            let args = params
                .get("args")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let env = params
                .get("env")
                .and_then(Value::as_array)
                .map(|entries| {
                    entries
                        .iter()
                        .filter_map(|entry| {
                            Some((
                                entry.get("name")?.as_str()?.to_string(),
                                entry.get("value")?.as_str()?.to_string(),
                            ))
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let output_byte_limit = params
                .get("outputByteLimit")
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok());
            let term_cwd = params
                .get("cwd")
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .unwrap_or(cwd);
            let id = inner
                .terminals
                .create(command, &args, term_cwd, &env, output_byte_limit)
                .await?;
            process.respond(req_id, json!({ "terminalId": id })).await?;
        }
        "terminal/output" => {
            let id = params
                .get("terminalId")
                .and_then(Value::as_str)
                .unwrap_or("");
            let output = inner
                .terminals
                .output(id)
                .unwrap_or(json!({ "output": "" }));
            process.respond(req_id, output).await?;
        }
        "terminal/wait_for_exit" => {
            let id = params
                .get("terminalId")
                .and_then(Value::as_str)
                .unwrap_or("");
            let status = inner.terminals.wait_for_exit(id).await?;
            process.respond(req_id, status).await?;
        }
        "terminal/kill" => {
            let id = params
                .get("terminalId")
                .and_then(Value::as_str)
                .unwrap_or("");
            let _ = inner.terminals.kill(id);
            process.respond(req_id, json!({})).await?;
        }
        "terminal/release" => {
            let id = params
                .get("terminalId")
                .and_then(Value::as_str)
                .unwrap_or("");
            inner.terminals.release(id);
            process.respond(req_id, json!({})).await?;
        }
        _ => {
            process
                .respond(req_id, json!({ "error": "unsupported" }))
                .await
                .ok();
        }
    }
    Ok(())
}

fn extra_env_for(def: &AcpAgentDef) -> Vec<(&'static str, String)> {
    let mut env = Vec::new();
    let home_key = match def.id.as_str() {
        "codex" => Some("CODEX_HOME"),
        "grok" => Some("GROK_HOME"),
        "claude" => Some("CLAUDE_CONFIG_DIR"),
        "opencode" => Some("OPENCODE_HOME"),
        _ => None,
    };
    if let Some(key) = home_key {
        if let Some(value) = std::env::var(key)
            .ok()
            .filter(|value| !value.trim().is_empty())
        {
            env.push((key, value));
        }
    }
    if def.id == "codex" {
        let codex_path = shell_words::split(&def.base_command)
            .ok()
            .and_then(|parts| parts.into_iter().next())
            .unwrap_or_else(|| def.base_command.clone());
        env.push(("CODEX_PATH", codex_path));
    }
    env
}

fn agent_server_command(def: &AcpAgentDef) -> String {
    if def.id == "grok" {
        "grok agent --always-approve --no-leader stdio".into()
    } else {
        def.server_command.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grok_always_auto_allows() {
        let grok = builtin_agents(None)
            .into_iter()
            .find(|agent| agent.id == "grok")
            .unwrap();
        assert_eq!(
            agent_server_command(&grok),
            "grok agent --always-approve --no-leader stdio"
        );
    }
}
