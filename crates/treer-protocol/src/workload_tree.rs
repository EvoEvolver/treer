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
