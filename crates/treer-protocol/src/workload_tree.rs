use serde::{Deserialize, Deserializer, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkloadFolder {
    pub folder_id: String,
    pub name: String,
    pub parent_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkloadPlacement {
    pub agent_id: String,
    pub folder_id: Option<String>,
    pub position: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkloadTree {
    pub folders: Vec<WorkloadFolder>,
    pub placements: Vec<WorkloadPlacement>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CreateWorkloadFolderRequest {
    pub name: String,
    pub parent_id: Option<String>,
}

fn nullable_parent<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(d).map(Some)
}

#[derive(Debug, Deserialize, Serialize)]
pub struct UpdateWorkloadFolderRequest {
    pub name: Option<String>,
    #[serde(
        default,
        deserialize_with = "nullable_parent",
        skip_serializing_if = "Option::is_none"
    )]
    pub parent_id: Option<Option<String>>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SetWorkloadPlacementRequest {
    pub folder_id: Option<String>,
    pub position: i64,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct DeleteWorkloadFolderQuery {
    #[serde(default)]
    pub require_empty: bool,
}

/// Workspace-shared Overview pins (#140): one list per workspace, keyed by
/// stable IDs, bounded, readable under Tree read and changed under Tree
/// manage. Names and paths are never part of this contract; the Console
/// resolves them from the Tree it is already authorised to read.
pub const WORKSPACE_PINS_LIMIT: usize = 50;
/// Advertised by the Proxy health document and `/api/auth/config`; an older
/// Proxy hides the Pin menu items instead of falling back to local storage.
pub const CAPABILITY_WORKSPACE_PINS: &str = "workspace.pins.v1";
/// Workspace event name: an IDs-only signal; clients re-read `GET /pins`.
pub const WORKSPACE_PINS_CHANGED_EVENT: &str = "workspace.pins.changed";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspacePinKind {
    Agent,
    App,
    Folder,
}

impl WorkspacePinKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::App => "app",
            Self::Folder => "folder",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "agent" => Self::Agent,
            "app" => Self::App,
            "folder" => Self::Folder,
            _ => return None,
        })
    }
}

/// Pinned item IDs are opaque stable IDs with a bounded, printable charset.
pub fn workspace_pin_id_ok(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-'))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspacePin {
    pub kind: WorkspacePinKind,
    pub id: String,
    pub pinned_at: String,
    /// User ID of the member who pinned it; `None` once that user is deleted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_by: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspacePinsResponse {
    pub pins: Vec<WorkspacePin>,
    pub limit: usize,
    /// The caller currently passes the Tree manage rule (a UI hint only; the
    /// write routes authorise independently). False when Policy is unavailable.
    pub can_pin: bool,
    pub revision: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspacePinAction {
    Pinned,
    Unpinned,
}

/// Payload of `workspace.pins.changed`: identifiers only, no names, no actor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspacePinsChanged {
    pub action: WorkspacePinAction,
    pub kind: WorkspacePinKind,
    pub id: String,
    pub revision: u64,
}

#[cfg(test)]
mod pin_tests {
    use super::*;

    #[test]
    fn pin_kinds_and_ids_are_strict() {
        for kind in [
            WorkspacePinKind::Agent,
            WorkspacePinKind::App,
            WorkspacePinKind::Folder,
        ] {
            assert_eq!(WorkspacePinKind::parse(kind.as_str()), Some(kind));
            assert_eq!(
                serde_json::to_string(&kind).unwrap(),
                format!("\"{}\"", kind.as_str())
            );
        }
        assert_eq!(WorkspacePinKind::parse("machine"), None);
        assert!(workspace_pin_id_ok("ag_0123abcd"));
        assert!(workspace_pin_id_ok("folder_9f.e:1-x"));
        assert!(!workspace_pin_id_ok(""));
        assert!(!workspace_pin_id_ok(&"a".repeat(129)));
        assert!(!workspace_pin_id_ok("a/b"));
        assert!(!workspace_pin_id_ok("a b"));
        assert!(!workspace_pin_id_ok("ä"));
        let changed = WorkspacePinsChanged {
            action: WorkspacePinAction::Pinned,
            kind: WorkspacePinKind::Folder,
            id: "folder_1".into(),
            revision: 7,
        };
        assert_eq!(
            serde_json::to_value(&changed).unwrap(),
            serde_json::json!({"action":"pinned","kind":"folder","id":"folder_1","revision":7})
        );
    }
}
