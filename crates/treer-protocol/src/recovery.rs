use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeRestoreState {
    Attached,
    Started,
    SkippedStopped,
    IdentityRevoked,
    Failed,
    WaitingEndpoint,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeRestoreItem {
    pub workload_id: String,
    pub kind: String,
    pub state: RuntimeRestoreState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeRestoreReport {
    pub workspace_id: String,
    pub server_id: String,
    pub items: Vec<RuntimeRestoreItem>,
}
