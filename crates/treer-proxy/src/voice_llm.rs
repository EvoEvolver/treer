use std::time::Duration;

use serde_json::{json, Value};
use treer_protocol::{AgentCommand, AgentInfo, ProtocolError, ServerInfo, WorkspaceSnapshot};

use crate::audit::NewWorkspaceAuditEvent;
use crate::auth::{AuthStore, CurrentSession};
use crate::state::AppState;

const DEFAULT_BASE_URL: &str = "https://sub.lnz-study.com";
const DEFAULT_MODEL: &str = "gpt-5.6-luna";
const SKILL: &str = include_str!("../../../skills/treer-voice/SKILL.md");
const MAX_ROUNDS: usize = 8;
const MAX_TOOL_RESULT_CHARS: usize = 8_000;
const LLM_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireApi {
    Responses,
    Completions,
}

impl WireApi {
    fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "" | "responses" | "response" => Ok(Self::Responses),
            "completions" | "completion" | "chat" | "chat_completions" | "chat-completions" => {
                Ok(Self::Completions)
            }
            other => Err(format!("unsupported TREER_VOICE_LLM_WIRE_API '{other}'")),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Responses => "responses",
            Self::Completions => "completions",
        }
    }
}

#[derive(Clone)]
pub struct VoiceLlmConfig {
    base_url: String,
    wire_api: WireApi,
    api_key: Option<String>,
    model: String,
    client: reqwest::Client,
}

impl std::fmt::Debug for VoiceLlmConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VoiceLlmConfig")
            .field("base_url", &self.base_url)
            .field("wire_api", &self.wire_api)
            .field(
                "api_key_set",
                &self.api_key.as_ref().is_some_and(|key| !key.is_empty()),
            )
            .field("model", &self.model)
            .finish()
    }
}

impl VoiceLlmConfig {
    pub fn from_env() -> Self {
        let wire_api = env_nonempty("TREER_VOICE_LLM_WIRE_API")
            .map(|value| WireApi::parse(&value).unwrap_or(WireApi::Responses))
            .unwrap_or(WireApi::Responses);
        Self::new(
            env_nonempty("TREER_VOICE_LLM_BASE_URL")
                .unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
            wire_api,
            env_nonempty("TREER_VOICE_LLM_API_KEY"),
            env_nonempty("TREER_VOICE_LLM_MODEL").unwrap_or_else(|| DEFAULT_MODEL.to_string()),
        )
    }

    fn new(base_url: String, wire_api: WireApi, api_key: Option<String>, model: String) -> Self {
        Self {
            base_url,
            wire_api,
            api_key,
            model,
            client: reqwest::Client::builder()
                .timeout(LLM_TIMEOUT)
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
        }
    }

    #[cfg(test)]
    pub fn disabled() -> Self {
        Self::new(
            DEFAULT_BASE_URL.to_string(),
            WireApi::Responses,
            None,
            DEFAULT_MODEL.to_string(),
        )
    }

    #[cfg(test)]
    pub fn for_test(base_url: &str, wire_api: WireApi, api_key: &str, model: &str) -> Self {
        Self::new(
            base_url.to_string(),
            wire_api,
            Some(api_key.to_string()),
            model.to_string(),
        )
    }

    pub fn enabled(&self) -> bool {
        self.api_key.as_ref().is_some_and(|key| !key.is_empty())
    }

    pub fn status_json(&self) -> Value {
        json!({
            "enabled": self.enabled(),
            "wire_api": if self.enabled() { Some(self.wire_api.as_str()) } else { None },
            "model": if self.enabled() { Some(self.model.as_str()) } else { None },
        })
    }

    fn endpoint(&self) -> String {
        endpoint_url(&self.base_url, self.wire_api)
    }
}

#[derive(Debug)]
pub enum VoiceCommandError {
    Unavailable,
    EmptyUtterance,
    Upstream(String),
    Protocol(ProtocolError),
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct VoiceHistoryTurn {
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub text: String,
}

#[derive(Clone, Copy)]
pub struct VoiceCommandRequest<'a> {
    pub config: &'a VoiceLlmConfig,
    pub state: &'a AppState,
    pub auth: &'a AuthStore,
    pub session: &'a CurrentSession,
    pub workspace_id: &'a str,
    pub utterance: &'a str,
    pub history: &'a [VoiceHistoryTurn],
}

#[derive(Debug, Clone)]
pub struct VoiceCommandResult {
    pub reply: String,
    pub utterance: String,
    pub tools: Vec<Value>,
}

impl VoiceCommandResult {
    pub fn to_json(&self) -> Value {
        json!({
            "reply": self.reply,
            "utterance": self.utterance,
            "tools": self.tools,
        })
    }
}

pub async fn run_voice_command(
    request: VoiceCommandRequest<'_>,
) -> Result<VoiceCommandResult, VoiceCommandError> {
    if !request.config.enabled() {
        return Err(VoiceCommandError::Unavailable);
    }
    let utterance = request.utterance.trim();
    if utterance.is_empty() {
        return Err(VoiceCommandError::EmptyUtterance);
    }
    tracing::info!(
        workspace_id = request.workspace_id,
        chars = utterance.chars().count(),
        model = %request.config.model,
        wire_api = request.config.wire_api.as_str(),
        "voice command started"
    );
    let snapshot = request
        .state
        .snapshot(request.workspace_id)
        .await
        .map_err(VoiceCommandError::Protocol)?;
    let instructions = instructions_for(&snapshot, request.session);
    match request.config.wire_api {
        WireApi::Responses => run_responses(request, utterance, instructions).await,
        WireApi::Completions => run_completions(request, utterance, instructions).await,
    }
}

fn instructions_for(snapshot: &WorkspaceSnapshot, session: &CurrentSession) -> String {
    format!(
        "{SKILL}\n\n## Current workspace roster\n\n{}",
        roster_text(snapshot, session)
    )
}

fn roster_text(snapshot: &WorkspaceSnapshot, session: &CurrentSession) -> String {
    let mut lines = vec![
        format!(
            "workspace / 工作空间: {} (id {})",
            snapshot.workspace.name, snapshot.workspace.workspace_id
        ),
        format!(
            "caller / 当前用户: {} (human, id {})",
            session.preferred_name, session.user_id
        ),
        "machines / 设备:".to_string(),
    ];
    if snapshot.servers.is_empty() {
        lines.push("- (none)".to_string());
    }
    for server in &snapshot.servers {
        lines.push(format!(
            "- {} (id {}, hostname {}, {})",
            display_or_dash(&server.name),
            server.server_id,
            server.hostname,
            format!("{:?}", server.status).to_ascii_lowercase()
        ));
    }
    lines.push("agents / 智能体:".to_string());
    let agents = snapshot
        .agents
        .iter()
        .filter(|agent| agent.kind != "app")
        .collect::<Vec<_>>();
    if agents.is_empty() {
        lines.push("- (none)".to_string());
    }
    for agent in agents {
        let machine = snapshot
            .servers
            .iter()
            .find(|server| server.server_id == agent.server_id)
            .map(|server| {
                if server.name.is_empty() {
                    server.hostname.as_str()
                } else {
                    server.name.as_str()
                }
            })
            .unwrap_or(agent.server_id.as_str());
        lines.push(format!(
            "- {} on {} (id {}, kind {}, {})",
            agent.name,
            machine,
            agent.agent_id,
            agent.kind,
            format!("{:?}", agent.status).to_ascii_lowercase()
        ));
    }
    lines.join("\n")
}

fn display_or_dash(value: &str) -> &str {
    if value.is_empty() {
        "-"
    } else {
        value
    }
}

fn treer_tool() -> Value {
    json!({
        "type": "function",
        "name": "treer",
        "description": "Run a Treer CLI command in the current workspace. Pass argv without the binary name, for example [\"status\"] or [\"agent\",\"prompt\",\"reviewer\",\"write tests\"]. Optional --machine <name-or-id> scopes agent commands to a device.",
        "parameters": {
            "type": "object",
            "properties": {
                "argv": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "CLI argv after `treer`, e.g. [\"agent\",\"list\",\"--machine\",\"mac\"]"
                },
                "command": {
                    "type": "string",
                    "description": "Alternative to argv: a single command string such as 'agent prompt reviewer write tests'"
                }
            }
        }
    })
}

fn completions_tool() -> Value {
    let tool = treer_tool();
    json!({
        "type": "function",
        "function": {
            "name": tool["name"],
            "description": tool["description"],
            "parameters": tool["parameters"]
        }
    })
}

const MAX_HISTORY_TURNS: usize = 12;
const MAX_HISTORY_CHARS: usize = 4_000;

pub(crate) fn compact_history(history: &[VoiceHistoryTurn]) -> Vec<Value> {
    let mut turns: Vec<(String, String)> = history
        .iter()
        .filter_map(|turn| {
            let role = turn.role.trim().to_ascii_lowercase();
            if role != "user" && role != "assistant" {
                return None;
            }
            let text = turn.text.trim();
            if text.is_empty() {
                return None;
            }
            Some((role, text.to_string()))
        })
        .collect();
    if turns.len() > MAX_HISTORY_TURNS {
        turns = turns.split_off(turns.len() - MAX_HISTORY_TURNS);
    }
    let mut chars = 0usize;
    let mut kept = Vec::new();
    for (role, text) in turns.into_iter().rev() {
        let next = chars.saturating_add(text.chars().count());
        if !kept.is_empty() && next > MAX_HISTORY_CHARS {
            break;
        }
        chars = next;
        kept.push((role, text));
    }
    kept.reverse();
    kept.into_iter()
        .map(|(role, text)| json!({ "role": role, "content": text }))
        .collect()
}

fn conversation_input(history: &[VoiceHistoryTurn], utterance: &str) -> Vec<Value> {
    let mut input = compact_history(history);
    if input.last().is_some_and(|item| {
        item.get("role").and_then(Value::as_str) == Some("user")
            && item.get("content").and_then(Value::as_str) == Some(utterance)
    }) {
        return input;
    }
    input.push(json!({ "role": "user", "content": utterance }));
    input
}

async fn run_responses(
    request: VoiceCommandRequest<'_>,
    utterance: &str,
    instructions: String,
) -> Result<VoiceCommandResult, VoiceCommandError> {
    let mut input = conversation_input(request.history, utterance);
    let mut tools = Vec::new();
    for _ in 0..MAX_ROUNDS {
        let body = json!({
            "model": request.config.model,
            "instructions": instructions,
            "input": input,
            "tools": [treer_tool()],
            "parallel_tool_calls": false,
        });
        let response = post_llm(request.config, body).await?;
        let parsed = parse_responses_output(&response);
        if parsed.calls.is_empty() {
            return Ok(VoiceCommandResult {
                reply: speakable_reply(parsed.text),
                utterance: utterance.to_string(),
                tools,
            });
        }
        input.extend(parsed.echo);
        for call in parsed.calls {
            let (ok, output) = execute_tool(request, &call.arguments).await;
            tools.push(json!({
                "argv": output.get("argv").cloned().unwrap_or(json!([])),
                "ok": ok,
            }));
            input.push(call.echo);
            input.push(json!({
                "type": "function_call_output",
                "call_id": call.call_id,
                "output": truncate_json(&output),
            }));
        }
    }
    Ok(VoiceCommandResult {
        reply: "已经开始处理，但这一轮还没有生成可朗读的回复。".to_string(),
        utterance: utterance.to_string(),
        tools,
    })
}

async fn run_completions(
    request: VoiceCommandRequest<'_>,
    utterance: &str,
    instructions: String,
) -> Result<VoiceCommandResult, VoiceCommandError> {
    let mut messages = vec![json!({"role": "system", "content": instructions})];
    messages.extend(conversation_input(request.history, utterance));
    let mut tools = Vec::new();
    for _ in 0..MAX_ROUNDS {
        let body = json!({
            "model": request.config.model,
            "messages": messages,
            "tools": [completions_tool()],
        });
        let response = post_llm(request.config, body).await?;
        let parsed = parse_completions_output(&response)?;
        if parsed.calls.is_empty() {
            return Ok(VoiceCommandResult {
                reply: speakable_reply(parsed.text),
                utterance: utterance.to_string(),
                tools,
            });
        }
        messages.push(parsed.assistant);
        for call in parsed.calls {
            let (ok, output) = execute_tool(request, &call.arguments).await;
            tools.push(json!({
                "argv": output.get("argv").cloned().unwrap_or(json!([])),
                "ok": ok,
            }));
            messages.push(json!({
                "role": "tool",
                "tool_call_id": call.call_id,
                "content": truncate_json(&output),
            }));
        }
    }
    Ok(VoiceCommandResult {
        reply: "已经开始处理，但这一轮还没有生成可朗读的回复。".to_string(),
        utterance: utterance.to_string(),
        tools,
    })
}

struct ParsedCall {
    call_id: String,
    arguments: Value,
    echo: Value,
}

struct ResponsesParse {
    calls: Vec<ParsedCall>,
    text: Option<String>,
    echo: Vec<Value>,
}

struct CompletionsParse {
    calls: Vec<ParsedCall>,
    text: Option<String>,
    assistant: Value,
}

fn parse_responses_output(response: &Value) -> ResponsesParse {
    let mut calls = Vec::new();
    let mut text = None;
    let mut echo = Vec::new();
    if let Some(items) = response.get("output").and_then(Value::as_array) {
        for item in items {
            match item.get("type").and_then(Value::as_str) {
                Some("function_call") => {
                    if let Some(call) = function_call_from_item(item) {
                        calls.push(call);
                    }
                }
                Some("message") => {
                    if text.is_none() {
                        text = message_text(item);
                    }
                    echo.push(item.clone());
                }
                _ => echo.push(item.clone()),
            }
        }
    }
    if text.is_none() {
        text = response
            .get("output_text")
            .and_then(Value::as_str)
            .map(str::to_string);
    }
    ResponsesParse { calls, text, echo }
}

fn parse_completions_output(response: &Value) -> Result<CompletionsParse, VoiceCommandError> {
    let message = response
        .pointer("/choices/0/message")
        .cloned()
        .ok_or_else(|| {
            VoiceCommandError::Upstream("completions response missing message".into())
        })?;
    let mut calls = Vec::new();
    if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
        for (index, item) in tool_calls.iter().enumerate() {
            let call_id = item
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("call_{index}"));
            let arguments = item
                .pointer("/function/arguments")
                .cloned()
                .unwrap_or(json!({}));
            calls.push(ParsedCall {
                call_id,
                arguments: parse_arguments(&arguments),
                echo: item.clone(),
            });
        }
    }
    let text = message
        .get("content")
        .and_then(Value::as_str)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    Ok(CompletionsParse {
        calls,
        text,
        assistant: message,
    })
}

fn function_call_from_item(item: &Value) -> Option<ParsedCall> {
    let call_id = item
        .get("call_id")
        .or_else(|| item.get("id"))
        .and_then(Value::as_str)?
        .to_string();
    let arguments = item.get("arguments").cloned().unwrap_or(json!({}));
    Some(ParsedCall {
        call_id,
        arguments: parse_arguments(&arguments),
        echo: item.clone(),
    })
}

fn parse_arguments(value: &Value) -> Value {
    match value {
        Value::String(raw) => serde_json::from_str(raw).unwrap_or_else(|_| json!({"command": raw})),
        other => other.clone(),
    }
}

fn message_text(item: &Value) -> Option<String> {
    let content = item.get("content")?;
    if let Some(text) = content.as_str() {
        let text = text.trim();
        return (!text.is_empty()).then(|| text.to_string());
    }
    let mut parts = Vec::new();
    for block in content.as_array()? {
        if block.get("type").and_then(Value::as_str) == Some("output_text")
            || block.get("type").and_then(Value::as_str) == Some("text")
        {
            if let Some(text) = block.get("text").and_then(Value::as_str) {
                if !text.trim().is_empty() {
                    parts.push(text.trim().to_string());
                }
            }
        }
    }
    let joined = parts.join(" ");
    (!joined.is_empty()).then_some(joined)
}

fn speakable_reply(text: Option<String>) -> String {
    text.filter(|value| !value.is_empty())
        .unwrap_or_else(|| "已经处理完了。".to_string())
}

async fn post_llm(config: &VoiceLlmConfig, body: Value) -> Result<Value, VoiceCommandError> {
    let api_key = config
        .api_key
        .as_deref()
        .ok_or(VoiceCommandError::Unavailable)?;
    let response = config
        .client
        .post(config.endpoint())
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await
        .map_err(|error| {
            VoiceCommandError::Upstream(format!("voice llm connect failed: {error}"))
        })?;
    let status = response.status();
    let payload = response
        .text()
        .await
        .map_err(|error| VoiceCommandError::Upstream(format!("voice llm read failed: {error}")))?;
    if !status.is_success() {
        return Err(VoiceCommandError::Upstream(format!(
            "voice llm HTTP {status}: {}",
            payload.chars().take(400).collect::<String>()
        )));
    }
    serde_json::from_str(&payload).map_err(|error| {
        VoiceCommandError::Upstream(format!("voice llm returned invalid JSON: {error}"))
    })
}

async fn execute_tool(request: VoiceCommandRequest<'_>, arguments: &Value) -> (bool, Value) {
    let argv = match argv_from_arguments(arguments) {
        Ok(argv) => argv,
        Err(message) => {
            return (
                false,
                json!({"error": {"code": "invalid_tool_arguments", "message": message}}),
            );
        }
    };
    match execute_treer(request, &argv).await {
        Ok(mut output) => {
            if let Some(object) = output.as_object_mut() {
                object.insert("argv".into(), json!(argv));
            }
            (true, output)
        }
        Err(error) => (
            false,
            json!({
                "argv": argv,
                "error": {"code": error.code, "message": error.message}
            }),
        ),
    }
}

fn argv_from_arguments(arguments: &Value) -> Result<Vec<String>, String> {
    if let Some(argv) = arguments.get("argv").and_then(Value::as_array) {
        return Ok(argv
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect());
    }
    if let Some(command) = arguments.get("command").and_then(Value::as_str) {
        return Ok(split_command(command));
    }
    Err("treer tool requires argv or command".to_string())
}

#[derive(Debug, PartialEq, Eq)]
enum TreerInvocation {
    Whoami,
    Status,
    MachineList,
    AgentList {
        machine: Option<String>,
    },
    AgentShow {
        target: String,
        machine: Option<String>,
    },
    AgentPrompt {
        target: String,
        text: String,
        machine: Option<String>,
    },
    AgentRead {
        target: String,
        lines: Option<usize>,
        machine: Option<String>,
    },
}

fn parse_treer(argv: &[String]) -> Result<TreerInvocation, ProtocolError> {
    let mut parts = argv.to_vec();
    if parts.first().map(String::as_str) == Some("treer") {
        parts.remove(0);
    }
    if parts.is_empty() {
        return Err(ProtocolError::new(
            "invalid_treer_command",
            "treer argv is empty",
        ));
    }
    match parts[0].as_str() {
        "whoami" if parts.len() == 1 => Ok(TreerInvocation::Whoami),
        "status" if parts.len() == 1 => Ok(TreerInvocation::Status),
        "machine" | "machines" | "server" | "servers" => {
            if parts.get(1).map(String::as_str) == Some("list") {
                Ok(TreerInvocation::MachineList)
            } else {
                forbidden(&parts)
            }
        }
        "agent" | "agents" => parse_agent(&parts[1..]),
        _ => forbidden(&parts),
    }
}

fn parse_agent(args: &[String]) -> Result<TreerInvocation, ProtocolError> {
    let Some(verb) = args.first().map(String::as_str) else {
        return Err(ProtocolError::new(
            "invalid_treer_command",
            "agent subcommand required",
        ));
    };
    let mut flags = FlagSet::parse(&args[1..])?;
    match verb {
        "list" => {
            if !flags.rest.is_empty() {
                return Err(ProtocolError::new(
                    "invalid_treer_command",
                    "agent list does not take a target",
                ));
            }
            Ok(TreerInvocation::AgentList {
                machine: flags.machine,
            })
        }
        "show" => Ok(TreerInvocation::AgentShow {
            target: flags.take_target("agent show")?,
            machine: flags.machine,
        }),
        "read" => Ok(TreerInvocation::AgentRead {
            target: flags.take_target("agent read")?,
            lines: flags.lines,
            machine: flags.machine,
        }),
        "prompt" => {
            let target = flags.take_target("agent prompt")?;
            if flags.rest.is_empty() {
                return Err(ProtocolError::new(
                    "invalid_treer_command",
                    "agent prompt requires task text",
                ));
            }
            Ok(TreerInvocation::AgentPrompt {
                target,
                text: flags.rest.join(" "),
                machine: flags.machine,
            })
        }
        _ => forbidden(&{
            let mut parts = vec!["agent".to_string()];
            parts.extend(args.iter().cloned());
            parts
        }),
    }
}

fn forbidden(parts: &[String]) -> Result<TreerInvocation, ProtocolError> {
    Err(ProtocolError::new(
        "voice_command_not_allowed",
        format!(
            "voice cannot run `treer {}`; allowed: status, whoami, machine list, agent list/show/prompt/read",
            parts.join(" ")
        ),
    ))
}

struct FlagSet {
    machine: Option<String>,
    lines: Option<usize>,
    rest: Vec<String>,
}

impl FlagSet {
    fn parse(args: &[String]) -> Result<Self, ProtocolError> {
        let mut machine = None;
        let mut lines = None;
        let mut rest = Vec::new();
        let mut index = 0;
        while index < args.len() {
            let arg = &args[index];
            if arg == "--" {
                rest.extend(args[index + 1..].iter().cloned());
                break;
            }
            if let Some(value) = arg.strip_prefix("--machine=") {
                machine = Some(value.to_string());
            } else if arg == "--machine" {
                index += 1;
                machine = Some(require_flag_value(args, index, "--machine")?);
            } else if let Some(value) = arg.strip_prefix("--lines=") {
                lines = Some(parse_lines(value)?);
            } else if arg == "--lines" {
                index += 1;
                lines = Some(parse_lines(&require_flag_value(args, index, "--lines")?)?);
            } else if arg == "--wait" || arg.starts_with("--timeout") {
                if arg == "--timeout" {
                    index += 1;
                }
            } else if arg.starts_with("--") {
                return Err(ProtocolError::new(
                    "invalid_treer_command",
                    format!("unsupported flag {arg}"),
                ));
            } else {
                rest.push(arg.clone());
            }
            index += 1;
        }
        Ok(Self {
            machine,
            lines,
            rest,
        })
    }

    fn take_target(&mut self, command: &str) -> Result<String, ProtocolError> {
        if self.rest.is_empty() {
            return Err(ProtocolError::new(
                "invalid_treer_command",
                format!("{command} requires an agent name or id"),
            ));
        }
        Ok(self.rest.remove(0))
    }
}

fn require_flag_value(args: &[String], index: usize, flag: &str) -> Result<String, ProtocolError> {
    args.get(index).cloned().ok_or_else(|| {
        ProtocolError::new("invalid_treer_command", format!("{flag} requires a value"))
    })
}

fn parse_lines(value: &str) -> Result<usize, ProtocolError> {
    value.parse().map_err(|_| {
        ProtocolError::new("invalid_treer_command", format!("invalid --lines {value}"))
    })
}

fn split_command(command: &str) -> Vec<String> {
    let trimmed = command.trim();
    let stripped = trimmed
        .strip_prefix("treer ")
        .or_else(|| (trimmed == "treer").then_some(""))
        .unwrap_or(trimmed);
    if let Some(rest) = stripped.strip_prefix("agent prompt ") {
        let tokens = tokenize(rest);
        let mut argv = vec!["agent".to_string(), "prompt".to_string()];
        argv.extend(tokens);
        return argv;
    }
    tokenize(stripped)
}

fn tokenize(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    for ch in input.chars() {
        if let Some(active) = quote {
            if ch == active {
                quote = None;
            } else {
                current.push(ch);
            }
            continue;
        }
        match ch {
            '"' | '\'' => quote = Some(ch),
            c if c.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

async fn execute_treer(
    request: VoiceCommandRequest<'_>,
    argv: &[String],
) -> Result<Value, ProtocolError> {
    let invocation = parse_treer(argv)?;
    let snapshot = request.state.snapshot(request.workspace_id).await?;
    match invocation {
        TreerInvocation::Whoami => Ok(json!({
            "workspace": snapshot.workspace,
            "caller": {
                "kind": "human",
                "user_id": request.session.user_id,
                "preferred_name": request.session.preferred_name,
            }
        })),
        TreerInvocation::Status | TreerInvocation::MachineList => {
            Ok(status_json(&snapshot, request.session, None))
        }
        TreerInvocation::AgentList { machine } => {
            let filter = match machine.as_deref() {
                Some(target) => {
                    Some(resolve_machine(request.state, request.workspace_id, target).await?)
                }
                None => None,
            };
            Ok(status_json(
                &snapshot,
                request.session,
                filter.as_ref().map(|server| server.server_id.as_str()),
            ))
        }
        TreerInvocation::AgentShow { target, machine } => {
            let agent = resolve_voice_agent(
                request.state,
                request.workspace_id,
                &target,
                machine.as_deref(),
            )
            .await?;
            Ok(agent_json(&snapshot, &agent))
        }
        TreerInvocation::AgentRead {
            target,
            lines,
            machine,
        } => {
            let agent = resolve_voice_agent(
                request.state,
                request.workspace_id,
                &target,
                machine.as_deref(),
            )
            .await?;
            request
                .state
                .send_command(
                    request.workspace_id,
                    &agent.server_id,
                    AgentCommand::Read {
                        agent_id: agent.agent_id,
                        lines: Some(lines.unwrap_or(80)),
                    },
                )
                .await
        }
        TreerInvocation::AgentPrompt {
            target,
            text,
            machine,
        } => {
            let agent = resolve_voice_agent(
                request.state,
                request.workspace_id,
                &target,
                machine.as_deref(),
            )
            .await?;
            let data = request
                .state
                .send_command(
                    request.workspace_id,
                    &agent.server_id,
                    AgentCommand::Prompt {
                        agent_id: agent.agent_id.clone(),
                        text,
                    },
                )
                .await?;
            if let Err(error) = request
                .auth
                .record_workspace_audit(NewWorkspaceAuditEvent {
                    workspace_id: request.workspace_id,
                    actor_kind: "user",
                    actor_id: Some(request.session.user_id.as_str()),
                    action: "agent.prompted",
                    resource_kind: "agent",
                    resource_id: &agent.agent_id,
                    resource_name: Some(&agent.name),
                    payload: json!({ "source": "voice", "server_id": agent.server_id }),
                })
                .await
            {
                tracing::warn!(?error, agent_id = %agent.agent_id, "failed to record voice prompt audit");
            }
            Ok(json!({
                "ok": true,
                "agent": agent_json(&snapshot, &agent),
                "result": data,
            }))
        }
    }
}

fn status_json(
    snapshot: &WorkspaceSnapshot,
    session: &CurrentSession,
    machine_id: Option<&str>,
) -> Value {
    let servers = snapshot
        .servers
        .iter()
        .filter(|server| machine_id.is_none_or(|id| server.server_id == id))
        .map(|server| {
            json!({
                "server_id": server.server_id,
                "name": server.name,
                "hostname": server.hostname,
                "status": server.status,
            })
        })
        .collect::<Vec<_>>();
    let agents = snapshot
        .agents
        .iter()
        .filter(|agent| agent.kind != "app")
        .filter(|agent| machine_id.is_none_or(|id| agent.server_id == id))
        .map(|agent| {
            json!({
                "agent_id": agent.agent_id,
                "name": agent.name,
                "kind": agent.kind,
                "status": agent.status,
                "server_id": agent.server_id,
                "machine": machine_name(snapshot, &agent.server_id),
            })
        })
        .collect::<Vec<_>>();
    json!({
        "workspace": snapshot.workspace,
        "caller": {
            "kind": "human",
            "user_id": session.user_id,
            "preferred_name": session.preferred_name,
        },
        "machines": servers,
        "agents": agents,
    })
}

fn agent_json(snapshot: &WorkspaceSnapshot, agent: &AgentInfo) -> Value {
    json!({
        "agent_id": agent.agent_id,
        "name": agent.name,
        "kind": agent.kind,
        "status": agent.status,
        "server_id": agent.server_id,
        "machine": machine_name(snapshot, &agent.server_id),
        "cwd": agent.cwd,
    })
}

fn machine_name(snapshot: &WorkspaceSnapshot, server_id: &str) -> String {
    snapshot
        .servers
        .iter()
        .find(|server| server.server_id == server_id)
        .map(|server| {
            if server.name.is_empty() {
                server.hostname.clone()
            } else {
                server.name.clone()
            }
        })
        .unwrap_or_else(|| server_id.to_string())
}

async fn resolve_machine(
    state: &AppState,
    workspace_id: &str,
    target: &str,
) -> Result<ServerInfo, ProtocolError> {
    match state.resolve_server(workspace_id, target).await {
        Ok(server) => Ok(server),
        Err(error) if error.code == "server_not_found" => {
            let snapshot = state.snapshot(workspace_id).await?;
            let needle = target.to_ascii_lowercase();
            let matches = snapshot
                .servers
                .iter()
                .filter(|server| machine_matches(server, &needle))
                .cloned()
                .collect::<Vec<_>>();
            match matches.as_slice() {
                [server] => Ok(server.clone()),
                [] => Err(error),
                _ => Err(ProtocolError::new(
                    "server_ambiguous",
                    format!("more than one machine matches {target}; use a server id"),
                )),
            }
        }
        Err(error) => Err(error),
    }
}

fn machine_matches(server: &ServerInfo, needle: &str) -> bool {
    server.name.to_ascii_lowercase() == needle
        || server.hostname.to_ascii_lowercase() == needle
        || server
            .hostname
            .split('.')
            .next()
            .is_some_and(|label| label.to_ascii_lowercase() == needle)
}

async fn resolve_voice_agent(
    state: &AppState,
    workspace_id: &str,
    target: &str,
    machine: Option<&str>,
) -> Result<AgentInfo, ProtocolError> {
    let machine = match machine {
        Some(target) => Some(resolve_machine(state, workspace_id, target).await?),
        None => None,
    };
    match state.resolve_agent(workspace_id, target).await {
        Ok(agent) => {
            if agent.kind == "app" {
                return Err(ProtocolError::new("agent_not_found", target));
            }
            if let Some(server) = &machine {
                if agent.server_id != server.server_id {
                    return Err(ProtocolError::new(
                        "agent_not_found",
                        format!("{target} is not on {}", server.name),
                    ));
                }
            }
            Ok(agent)
        }
        Err(error) if error.code == "agent_not_found" || error.code == "agent_ambiguous" => {
            let snapshot = state.snapshot(workspace_id).await?;
            let needle = target.to_ascii_lowercase();
            let matches = snapshot
                .agents
                .iter()
                .filter(|agent| agent.kind != "app")
                .filter(|agent| {
                    machine
                        .as_ref()
                        .is_none_or(|server| agent.server_id == server.server_id)
                })
                .filter(|agent| {
                    agent.name.to_ascii_lowercase() == needle || agent.agent_id == target
                })
                .cloned()
                .collect::<Vec<_>>();
            match matches.as_slice() {
                [agent] => Ok(agent.clone()),
                [] => Err(error),
                _ => Err(ProtocolError::new(
                    "agent_ambiguous",
                    format!("more than one agent is named {target}; use an agent id"),
                )),
            }
        }
        Err(error) => Err(error),
    }
}

fn truncate_json(value: &Value) -> String {
    let encoded = serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string());
    if encoded.len() <= MAX_TOOL_RESULT_CHARS {
        encoded
    } else {
        format!(
            "{}…",
            encoded
                .chars()
                .take(MAX_TOOL_RESULT_CHARS)
                .collect::<String>()
        )
    }
}

fn endpoint_url(base_url: &str, wire_api: WireApi) -> String {
    let base = base_url.trim().trim_end_matches('/');
    let path = match wire_api {
        WireApi::Responses => "responses",
        WireApi::Completions => "chat/completions",
    };
    if base.ends_with(path) {
        base.to_string()
    } else if base.ends_with("/v1") {
        format!("{base}/{path}")
    } else {
        format!("{base}/v1/{path}")
    }
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
pub(crate) async fn spawn_scripted_upstream(script: Vec<Value>) -> url::Url {
    use axum::routing::post;
    use axum::Router;
    use std::collections::VecDeque;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    let script = Arc::new(Mutex::new(VecDeque::from(script)));
    let app = Router::new()
        .route("/v1/responses", post(scripted_llm))
        .route("/v1/chat/completions", post(scripted_llm))
        .with_state(script);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind scripted llm");
    let addr = listener.local_addr().expect("scripted llm address");
    tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("scripted llm server");
    });
    tokio::task::yield_now().await;
    url::Url::parse(&format!("http://{addr}/")).expect("scripted llm URL")
}

#[cfg(test)]
async fn scripted_llm(
    axum::extract::State(script): axum::extract::State<
        std::sync::Arc<tokio::sync::Mutex<std::collections::VecDeque<Value>>>,
    >,
    axum::Json(_body): axum::Json<Value>,
) -> (axum::http::StatusCode, axum::Json<Value>) {
    use axum::http::StatusCode;
    match script.lock().await.pop_front() {
        Some(value) => (StatusCode::OK, axum::Json(value)),
        None => (
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(json!({"error": {"message": "script exhausted"}})),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::AuthStore;
    use chrono::Utc;
    use treer_protocol::{
        AgentServerSnapshot, AgentStatus, BuildInfo, CommandResult, ProxyMessage, ServerStatus,
        WorkspaceInfo,
    };
    use uuid::Uuid;

    fn session() -> CurrentSession {
        CurrentSession {
            token: "tok_test".to_string(),
            user_id: "user_voice".to_string(),
            email: "voice@example.com".to_string(),
            preferred_name: "Ada".to_string(),
        }
    }

    fn test_server(name: &str, hostname: &str) -> ServerInfo {
        let now = Utc::now();
        ServerInfo {
            server_id: format!("srv_{name}"),
            workspace_id: "default".to_string(),
            name: name.to_string(),
            hostname: hostname.to_string(),
            root: "/tmp".to_string(),
            controller_build: BuildInfo {
                version: "test".to_string(),
                git_commit: "test".to_string(),
            },
            host_build: BuildInfo {
                version: "test".to_string(),
                git_commit: "test".to_string(),
            },
            supervision: None,
            labels: Default::default(),
            available_agents: None,
            status: ServerStatus::Online,
            connected_at: now,
            last_seen_at: now,
        }
    }

    fn test_agent(id: &str, name: &str, server_id: &str) -> AgentInfo {
        let now = Utc::now();
        AgentInfo {
            agent_id: id.to_string(),
            workspace_id: "default".to_string(),
            server_id: server_id.to_string(),
            kind: "codex".to_string(),
            name: name.to_string(),
            cwd: ".".to_string(),
            status: AgentStatus::Idle,
            pid: None,
            started_at: now,
            updated_at: now,
            exited_at: None,
            exit_code: None,
            output_revision: 0,
            interface: None,
        }
    }

    async fn workspace_with_mac_reviewer() -> (
        AppState,
        tokio::sync::mpsc::UnboundedReceiver<crate::state::SocketFrame>,
    ) {
        let state = AppState::new();
        state
            .ensure_workspace_info(WorkspaceInfo {
                workspace_id: "default".to_string(),
                name: "lab".to_string(),
                created_at: Utc::now(),
            })
            .await;
        let server = test_server("mac", "MacBook-Pro.local");
        let agent = test_agent("ag_reviewer", "reviewer", &server.server_id);
        let connection_id = Uuid::new_v4();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        state
            .register_server(server.clone(), connection_id, tx)
            .await
            .expect("register server");
        state
            .apply_snapshot(
                connection_id,
                AgentServerSnapshot {
                    server,
                    agents: vec![agent],
                },
            )
            .await
            .expect("snapshot");
        (state, rx)
    }

    fn spawn_prompt_ok(
        state: AppState,
        mut rx: tokio::sync::mpsc::UnboundedReceiver<crate::state::SocketFrame>,
    ) {
        tokio::spawn(async move {
            while let Some(frame) = rx.recv().await {
                let crate::state::SocketFrame::Text(encoded) = frame else {
                    continue;
                };
                let ProxyMessage::Command { envelope } =
                    serde_json::from_str(&encoded).expect("command")
                else {
                    continue;
                };
                let data = match envelope.command {
                    AgentCommand::Prompt { text, .. } => json!({"ok": true, "prompted": text}),
                    AgentCommand::Read { .. } => json!({"text": "idle"}),
                    _ => json!({}),
                };
                state
                    .complete_command(CommandResult::success(envelope.command_id, data))
                    .await;
            }
        });
    }

    #[test]
    fn parses_voice_cli_argv() {
        assert_eq!(
            parse_treer(&["status".into()]).expect("status"),
            TreerInvocation::Status
        );
        assert_eq!(
            parse_treer(&[
                "agent".into(),
                "prompt".into(),
                "reviewer".into(),
                "写测试".into()
            ])
            .expect("prompt"),
            TreerInvocation::AgentPrompt {
                target: "reviewer".into(),
                text: "写测试".into(),
                machine: None,
            }
        );
        assert_eq!(
            parse_treer(&[
                "agent".into(),
                "prompt".into(),
                "--machine".into(),
                "mac".into(),
                "reviewer".into(),
                "fix".into(),
                "the".into(),
                "build".into()
            ])
            .expect("prompt machine"),
            TreerInvocation::AgentPrompt {
                target: "reviewer".into(),
                text: "fix the build".into(),
                machine: Some("mac".into()),
            }
        );
        assert!(parse_treer(&["agent".into(), "delete".into(), "reviewer".into()]).is_err());
        assert_eq!(
            split_command("agent prompt reviewer 请写一个测试"),
            vec!["agent", "prompt", "reviewer", "请写一个测试"]
        );
    }

    #[test]
    fn responses_endpoint_appends_v1() {
        assert_eq!(
            endpoint_url("https://sub.lnz-study.com", WireApi::Responses),
            "https://sub.lnz-study.com/v1/responses"
        );
        assert_eq!(
            endpoint_url("https://sub.lnz-study.com/v1", WireApi::Completions),
            "https://sub.lnz-study.com/v1/chat/completions"
        );
    }

    #[test]
    fn skill_covers_asr_and_bilingual_concepts() {
        assert!(SKILL.contains("automatic speech recognition"));
        assert!(SKILL.contains("workspace / 工作空间"));
        assert!(SKILL.contains("machine / 设备"));
        assert!(SKILL.contains("agent / Agent / 智能体"));
        assert!(SKILL.contains("multi-turn spoken session"));
        assert!(SKILL.contains("Do not repeat a"));
    }

    #[test]
    fn compact_history_keeps_recent_user_and_assistant_turns() {
        let history = vec![
            VoiceHistoryTurn {
                role: "user".into(),
                text: "上面有哪些 agent".into(),
            },
            VoiceHistoryTurn {
                role: "assistant".into(),
                text: "有两个同名的 Codex。你要哪一个？".into(),
            },
            VoiceHistoryTurn {
                role: "user".into(),
                text: "给第一个发送消息".into(),
            },
            VoiceHistoryTurn {
                role: "assistant".into(),
                text: "你想发什么内容？".into(),
            },
        ];
        let input = conversation_input(&history, "让他回复一个一");
        assert_eq!(input.len(), 5);
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[1]["content"], "有两个同名的 Codex。你要哪一个？");
        assert_eq!(input[3]["content"], "你想发什么内容？");
        assert_eq!(input[4]["content"], "让他回复一个一");
    }

    #[tokio::test]
    async fn tools_resolve_mac_and_prompt_reviewer() {
        let auth = AuthStore::for_test("admin-password").await;
        let (state, rx) = workspace_with_mac_reviewer().await;
        spawn_prompt_ok(state.clone(), rx);
        let session = session();
        let request = VoiceCommandRequest {
            config: &VoiceLlmConfig::disabled(),
            state: &state,
            auth: &auth,
            session: &session,
            workspace_id: "default",
            utterance: "unused",
            history: &[],
        };
        let listed = execute_treer(
            request,
            &[
                "agent".into(),
                "list".into(),
                "--machine".into(),
                "Mac".into(),
            ],
        )
        .await
        .expect("list");
        assert_eq!(listed["agents"][0]["name"], "reviewer");
        let prompted = execute_treer(
            request,
            &[
                "agent".into(),
                "prompt".into(),
                "--machine".into(),
                "mac".into(),
                "reviewer".into(),
                "写一个测试".into(),
            ],
        )
        .await
        .expect("prompt");
        assert_eq!(prompted["ok"], true);
        assert_eq!(prompted["agent"]["name"], "reviewer");
        assert_eq!(prompted["result"]["prompted"], "写一个测试");
    }

    #[tokio::test]
    async fn responses_loop_prompts_from_asr_utterance() {
        let auth = AuthStore::for_test("admin-password").await;
        let (state, rx) = workspace_with_mac_reviewer().await;
        spawn_prompt_ok(state.clone(), rx);
        let upstream = spawn_scripted_upstream(vec![
            json!({
                "id": "resp_1",
                "output": [{
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "treer",
                    "arguments": "{\"argv\":[\"agent\",\"prompt\",\"--machine\",\"mac\",\"reviewer\",\"给这个仓库写测试\"]}"
                }]
            }),
            json!({
                "id": "resp_2",
                "output": [{
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "已经把写测试的任务发给 Mac 上的 reviewer 了。"}]
                }]
            }),
        ])
        .await;
        let config = VoiceLlmConfig::for_test(
            upstream.as_str(),
            WireApi::Responses,
            "sk-test",
            "gpt-5.6-luna",
        );
        let session = session();
        let result = run_voice_command(VoiceCommandRequest {
            config: &config,
            state: &state,
            auth: &auth,
            session: &session,
            workspace_id: "default",
            utterance: "请看一下 mac 这个设备上运行的 reviewer agent，让它进行写测试",
            history: &[],
        })
        .await
        .expect("voice command");
        assert!(result.reply.contains("reviewer"));
        assert_eq!(result.tools.len(), 1);
        assert_eq!(result.tools[0]["ok"], true);
        assert_eq!(result.tools[0]["argv"][1], "prompt");
    }

    #[tokio::test]
    async fn completions_loop_lists_then_speaks() {
        let auth = AuthStore::for_test("admin-password").await;
        let (state, rx) = workspace_with_mac_reviewer().await;
        spawn_prompt_ok(state.clone(), rx);
        let upstream = spawn_scripted_upstream(vec![
            json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": "call_status",
                            "type": "function",
                            "function": {
                                "name": "treer",
                                "arguments": "{\"argv\":[\"status\"]}"
                            }
                        }]
                    }
                }]
            }),
            json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": "Mac 在线，上面的 reviewer 现在空闲。"
                    }
                }]
            }),
        ])
        .await;
        let config = VoiceLlmConfig::for_test(
            upstream.as_str(),
            WireApi::Completions,
            "sk-test",
            "gpt-5.6-luna",
        );
        let session = session();
        let result = run_voice_command(VoiceCommandRequest {
            config: &config,
            state: &state,
            auth: &auth,
            session: &session,
            workspace_id: "default",
            utterance: "看一下 mac 上有哪些 agent",
            history: &[],
        })
        .await
        .expect("completions command");
        assert!(result.reply.contains("reviewer"));
        assert_eq!(result.tools[0]["argv"][0], "status");
    }

    #[tokio::test]
    async fn live_responses_upstream_prompts_reviewer_from_asr_text() {
        if std::env::var("TREER_VOICE_LLM_LIVE_TEST").ok().as_deref() != Some("1") {
            return;
        }
        let api_key = std::env::var("TREER_VOICE_LLM_API_KEY").expect("TREER_VOICE_LLM_API_KEY");
        let base_url = std::env::var("TREER_VOICE_LLM_BASE_URL")
            .unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
        let model =
            std::env::var("TREER_VOICE_LLM_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());
        let auth = AuthStore::for_test("admin-password").await;
        let (state, rx) = workspace_with_mac_reviewer().await;
        spawn_prompt_ok(state.clone(), rx);
        let config = VoiceLlmConfig::for_test(&base_url, WireApi::Responses, &api_key, &model);
        let session = session();
        let result = run_voice_command(VoiceCommandRequest {
            config: &config,
            state: &state,
            auth: &auth,
            session: &session,
            workspace_id: "default",
            utterance: "请看一下 mac 这个设备上运行的 reviewer agent，让它进行写一个单元测试",
            history: &[],
        })
        .await
        .expect("live voice command");
        assert!(
            result.tools.iter().any(|tool| tool["ok"] == true
                && tool["argv"]
                    .as_array()
                    .is_some_and(|argv| argv.iter().any(|item| item == "prompt"))),
            "live LLM should prompt reviewer, got {:?}",
            result
        );
        assert!(!result.reply.trim().is_empty());
        assert!(
            !result.reply.contains('#'),
            "reply should be speakable: {}",
            result.reply
        );
        eprintln!("live reply: {}", result.reply);
        eprintln!(
            "live tools: {}",
            serde_json::to_string(&result.tools).unwrap()
        );
    }
}
