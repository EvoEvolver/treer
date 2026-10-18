//! Member-visible inventory views (#153). A browser session receives an
//! allow-listed Agent shape without `pid`; credentials that operate a machine
//! or run tooling (bearer tokens held by humans, Agent and machine tokens)
//! keep the full `AgentInfo`, still filtered by Policy `agent.discover` and
//! workspace membership upstream. Every path that returns inventory to a
//! client goes through this module so an unknown credential kind cannot leak
//! the full shape.
use axum::http::{header, HeaderMap};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use treer_protocol::{
    AgentInfo, AgentInterfaceDescriptor, AgentStatus, WorkspaceEvent, WorkspaceSnapshot,
};

/// How much of the Agent inventory the caller may see.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InventoryView {
    /// A human browser session (cookie): no `pid`.
    Browser,
    /// A bearer credential (human CLI or API token, Agent, machine): full.
    Trusted,
}

/// Decides the view from the credential that authenticated the request: a
/// `Bearer` token is a tooling credential, anything else (the session cookie,
/// an unknown mechanism) is the browser class.
pub(crate) fn inventory_view(headers: &HeaderMap) -> InventoryView {
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.len() > 7 && value[..7].eq_ignore_ascii_case("bearer "));
    if bearer {
        InventoryView::Trusted
    } else {
        InventoryView::Browser
    }
}

/// The exact fields a browser session may see; `pid` is deliberately absent.
#[derive(Serialize)]
struct BrowserAgentInfo<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    path: &'a Option<String>,
    agent_id: &'a str,
    run_generation: u64,
    workspace_id: &'a str,
    server_id: &'a str,
    kind: &'a str,
    name: &'a str,
    cwd: &'a str,
    status: &'a AgentStatus,
    started_at: &'a DateTime<Utc>,
    updated_at: &'a DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exited_at: &'a Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: &'a Option<i32>,
    output_revision: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    interface: &'a Option<AgentInterfaceDescriptor>,
}

impl<'a> From<&'a AgentInfo> for BrowserAgentInfo<'a> {
    fn from(agent: &'a AgentInfo) -> Self {
        Self {
            path: &agent.path,
            agent_id: &agent.agent_id,
            run_generation: agent.run_generation,
            workspace_id: &agent.workspace_id,
            server_id: &agent.server_id,
            kind: &agent.kind,
            name: &agent.name,
            cwd: &agent.cwd,
            status: &agent.status,
            started_at: &agent.started_at,
            updated_at: &agent.updated_at,
            exited_at: &agent.exited_at,
            exit_code: &agent.exit_code,
            output_revision: agent.output_revision,
            interface: &agent.interface,
        }
    }
}

pub(crate) fn agent_value(view: InventoryView, agent: &AgentInfo) -> serde_json::Result<Value> {
    match view {
        InventoryView::Browser => serde_json::to_value(BrowserAgentInfo::from(agent)),
        InventoryView::Trusted => serde_json::to_value(agent),
    }
}

pub(crate) fn agents_value(
    view: InventoryView,
    agents: &[AgentInfo],
) -> serde_json::Result<Vec<Value>> {
    agents
        .iter()
        .map(|agent| agent_value(view, agent))
        .collect()
}

pub(crate) fn snapshot_value(
    view: InventoryView,
    snapshot: &WorkspaceSnapshot,
) -> serde_json::Result<Value> {
    let mut value = serde_json::to_value(snapshot)?;
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "agents".to_string(),
            Value::Array(agents_value(view, &snapshot.agents)?),
        );
    }
    Ok(value)
}

/// Re-serializes an event for the view at delivery time, so stored or
/// buffered events that already contain `pid` never reach a browser.
pub(crate) fn event_for_view(view: InventoryView, event: &WorkspaceEvent) -> WorkspaceEvent {
    if view == InventoryView::Trusted {
        return event.clone();
    }
    let mut redacted = event.clone();
    if event.event == "workspace.snapshot" {
        if let Ok(snapshot) = serde_json::from_value::<WorkspaceSnapshot>(event.data.clone()) {
            redacted.data = snapshot_value(view, &snapshot).unwrap_or(Value::Null);
        } else {
            redacted.data = Value::Null;
        }
    } else if event.event.starts_with("agent.") {
        if let Ok(agent) = serde_json::from_value::<AgentInfo>(event.data.clone()) {
            redacted.data = agent_value(view, &agent).unwrap_or(Value::Null);
        } else if let Some(object) = redacted.data.as_object_mut() {
            // Not an AgentInfo payload (for example an id-only event): drop a
            // pid key defensively, keep the rest.
            object.remove("pid");
        }
    }
    redacted
}
