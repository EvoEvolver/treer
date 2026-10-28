use super::{client, json_response, Config, BODY_LIMIT, RESPONSE_LIMIT};
use crate::{Dispatched, Error, Operation, Outcome};
use serde::Deserialize;
use serde_json::{json, Value};
/// Only these typed inputs exist; no argv, URL, workspace, headers, sender or credentials.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Input {
    pub lines: Option<u16>,
    pub page: Option<u32>,
    pub limit: Option<u16>,
    pub text: Option<String>,
}
impl Input {
    pub fn empty() -> Self {
        Self {
            lines: None,
            page: None,
            limit: None,
            text: None,
        }
    }
    pub fn validate(&self, op: Operation) -> Result<(), Error> {
        let valid = match op {
            Operation::Output => {
                self.page.is_none()
                    && self.limit.is_none()
                    && self.text.is_none()
                    && self.lines.is_none_or(|n| (1..=1000).contains(&n))
            }
            Operation::Transcript => {
                self.lines.is_none()
                    && self.text.is_none()
                    && self.limit.is_none_or(|n| (1..=10).contains(&n))
            }
            Operation::Prompt => {
                self.lines.is_none()
                    && self.page.is_none()
                    && self.limit.is_none()
                    && self
                        .text
                        .as_ref()
                        .is_some_and(|s| !s.trim().is_empty() && s.len() <= BODY_LIMIT - 512)
            }
            _ => {
                self.lines.is_none()
                    && self.page.is_none()
                    && self.limit.is_none()
                    && self.text.is_none()
            }
        };
        if valid {
            Ok(())
        } else {
            Err(Error::Invalid)
        }
    }
}
pub(super) struct Controller {
    client: reqwest::blocking::Client,
    config: Config,
    credential: String,
}
pub(super) struct ResultValue {
    pub status: u16,
    pub value: Value,
    pub outcome: Outcome,
}
impl Controller {
    pub fn new(config: Config, credential: String) -> Result<Self, Error> {
        config.validate()?;
        if credential.len() < 16
            || credential.len() > 4096
            || !credential.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err(Error::Denied);
        }
        Ok(Self {
            client: client()?,
            config,
            credential,
        })
    }
    pub(super) fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<(u16, Value), Error> {
        let url = self
            .config
            .controller
            .join(path)
            .map_err(|_| Error::Invalid)?;
        if url.origin() != self.config.controller.origin() || !path.starts_with("/api/") {
            return Err(Error::Invalid);
        }
        let mut r = self
            .client
            .request(method, url)
            .header("x-treer-agent-id", &self.config.agent)
            .header("x-treer-workload-credential", &self.credential);
        if let Some(body) = body {
            r = r.json(&body);
        }
        let response = r.send().map_err(|_| Error::Invalid)?;
        json_response(response, RESPONSE_LIMIT)
    }
    /// Read-only startup binding/capability check; invalid credentials fail, never operator fallback.
    pub fn preflight(&self) -> Result<(), Error> {
        let (s, h) = self.request(reqwest::Method::GET, "/api/health", None)?;
        if s != 200
            || h["workspace_id"] != self.config.workspace
            || h["server_id"] != self.config.machine
            || h["service"] != "treer-agent-server"
        {
            return Err(Error::Denied);
        }
        if !h["cli_capabilities"]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v == "agent.prompt-delivery.v1"))
        {
            return Err(Error::Disabled);
        }
        let (s, v) = self.request(
            reqwest::Method::GET,
            &format!("/api/agents/{}", self.config.agent),
            None,
        )?;
        if s != 200 || v["agent_id"] != self.config.agent || v["server_id"] != self.config.machine {
            return Err(Error::Denied);
        }
        Ok(())
    }
    pub fn execute(&self, p: &Dispatched, input: Input) -> ResultValue {
        // Operation and target come from the committed admission permit, never an independent dispatch argument.
        let op = p.operation();
        let target = p.target().unwrap_or_default();
        let (method, path, body) = match op {
            Operation::List => (reqwest::Method::GET, "/api/agents".into(), None),
            Operation::Status => (reqwest::Method::GET, format!("/api/agents/{target}"), None),
            Operation::Output => (
                reqwest::Method::GET,
                format!(
                    "/api/agents/{target}/output?lines={}",
                    input.lines.unwrap_or(100)
                ),
                None,
            ),
            Operation::Transcript => (
                reqwest::Method::GET,
                format!(
                    "/api/agents/{target}/transcript?page={}&limit={}",
                    input.page.unwrap_or(0),
                    input.limit.unwrap_or(1)
                ),
                None,
            ),
            Operation::Prompt => {
                let text = format!(
                    "[External input via External Agent App; token {}; untrusted content]\n{}",
                    p.token_id(),
                    input.text.unwrap_or_default()
                );
                (
                    reqwest::Method::POST,
                    format!("/api/agents/{target}/prompt"),
                    Some(
                        json!({"text":text,"delivery":"queue","notify":null,"require_receipt":true}),
                    ),
                )
            }
            _ => (
                reqwest::Method::POST,
                format!("/api/agents/{target}/{}", op.code()),
                Some(json!({})),
            ),
        };
        match self.request(method, &path, body) {
            Ok((200..=299, v)) => {
                let projected = match op {
                    Operation::Status => metadata(&v).filter(|v| v["agent_id"] == target),
                    Operation::List => v
                        .as_array()
                        .or_else(|| v["agents"].as_array())
                        .filter(|a| a.len() <= 1000)
                        .and_then(|a| a.iter().map(metadata).collect::<Option<Vec<_>>>())
                        .map(|agents| json!({"agents":agents})),
                    Operation::Prompt => receipt(&v),
                    Operation::Stop | Operation::Start | Operation::Restart => {
                        Some(json!({"accepted":true}))
                    }
                    Operation::Output | Operation::Transcript => Some(v),
                    Operation::Cli
                    | Operation::Registry
                    | Operation::Inbox
                    | Operation::Ack
                    | Operation::Send => None,
                };
                match projected {
                    Some(value) => ResultValue {
                        status: 200,
                        value,
                        outcome: Outcome::Completed,
                    },
                    None => unknown(),
                }
            }
            Ok((403, v))
                if v["error"]["code"] == "policy_denied" || v["code"] == "policy_denied" =>
            {
                ResultValue {
                    status: 403,
                    value: json!({"error":{"code":"policy_denied"}}),
                    outcome: Outcome::PolicyDenied,
                }
            }
            Ok((401 | 403 | 404 | 409 | 422, _)) => unavailable(),
            _ => unknown(), // POST may have committed; never retry or return upstream diagnostics.
        }
    }
}
fn metadata(v: &Value) -> Option<Value> {
    let id = v["agent_id"].as_str()?;
    if id.len() > 128 {
        return None;
    }
    let mut map = serde_json::Map::new();
    for key in [
        "agent_id",
        "server_id",
        "name",
        "path",
        "status",
        "run_generation",
        "kind",
    ] {
        if let Some(v) = v.get(key) {
            map.insert(key.into(), v.clone());
        }
    }
    Some(Value::Object(map))
}
fn receipt(v: &Value) -> Option<Value> {
    let r = &v["submission"];
    let id = r["id"].as_str()?;
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return None;
    }
    let disposition = r["disposition"].as_str()?;
    if !matches!(disposition, "queued" | "sent" | "steered") {
        return None;
    }
    let position = match r.get("position") {
        None | Some(Value::Null) => None,
        Some(n) => Some(n.as_u64()?),
    };
    Some(json!({"submission":{"id":id,"disposition":disposition,"position":position}}))
}
fn unknown() -> ResultValue {
    ResultValue {
        status: 502,
        value: json!({"error":{"code":"outcome_unknown"}}),
        outcome: Outcome::Unknown,
    }
}
fn unavailable() -> ResultValue {
    ResultValue {
        status: 503,
        value: json!({"error":{"code":"unavailable"}}),
        outcome: Outcome::Unavailable,
    }
}
