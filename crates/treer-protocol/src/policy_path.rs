use serde::{Deserialize, Serialize};

/// Current canonical Tree facts, populated by Core rather than clients or bundles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyTreeContext {
    pub node_id: String,
    pub kind: String,
    pub name: String,
    pub path: String,
    pub folder_id: Option<String>,
    pub folder_path: String,
    pub ancestor_ids: Vec<String>,
    pub ancestor_paths: Vec<String>,
    pub placed: bool,
    pub tree_revision: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyTreeSelector {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_eq: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_glob: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_prefix: Option<PolicyPathPrefix>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyPathPrefix {
    pub path: String,
    pub include_self: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PolicyFolderField {
    #[serde(rename = "subject.tree.folder")]
    Subject,
    #[serde(rename = "resource.tree.folder")]
    Resource,
}

/// Exactly one typed folder reference; no interpolation or arbitrary expressions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PolicyFolderRef {
    Variable(PolicyFolderVariable),
    Field(PolicyFolderFieldRef),
    Id(PolicyFolderId),
    Path(PolicyFolderPath),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyFolderVariable {
    pub var: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyFolderFieldRef {
    #[serde(rename = "ref")]
    pub field: PolicyFolderField,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyFolderId {
    pub id: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyFolderPath {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum PolicyTreeCondition {
    SameFolder,
    ResourceBelowSubjectFolder {
        #[serde(default)]
        strict: bool,
    },
    ResourceBelowFolder {
        folder: PolicyFolderRef,
        #[serde(default)]
        strict: bool,
    },
}
