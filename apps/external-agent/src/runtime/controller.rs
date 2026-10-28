use super::{client, json_response, Config, RESPONSE_LIMIT};
use crate::{Error, Outcome};
use serde_json::Value;
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
    /// Read-only startup binding check; invalid credentials fail, never operator fallback.
    /// Returns whether the Controller advertises the endpoint inbox capability.
    pub fn preflight(&self) -> Result<bool, Error> {
        let (s, h) = self.request(reqwest::Method::GET, "/api/health", None)?;
        if s != 200
            || h["workspace_id"] != self.config.workspace
            || h["server_id"] != self.config.machine
            || h["service"] != "treer-agent-server"
        {
            return Err(Error::Denied);
        }
        let (s, v) = self.request(
            reqwest::Method::GET,
            &format!("/api/agents/{}", self.config.agent),
            None,
        )?;
        if s != 200 || v["agent_id"] != self.config.agent || v["server_id"] != self.config.machine {
            return Err(Error::Denied);
        }
        Ok(endpoint_inbox(&h))
    }
    /// Current Controller support for `message.endpoint-inbox.v1`.
    pub fn endpoint_inbox(&self) -> bool {
        self.request(reqwest::Method::GET, "/api/health", None)
            .is_ok_and(|(s, h)| s == 200 && endpoint_inbox(&h))
    }
}
pub(super) const ENDPOINT_INBOX: &str = "message.endpoint-inbox.v1";
fn endpoint_inbox(health: &Value) -> bool {
    health["cli_capabilities"]
        .as_array()
        .is_some_and(|a| a.iter().any(|v| v == ENDPOINT_INBOX))
}
