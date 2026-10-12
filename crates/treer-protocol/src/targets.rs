//! Workspace tree addressing. Identity and Policy always use stable IDs.
use crate::{ProtocolError, WorkloadTree};
use std::collections::HashSet;

#[derive(Clone, Debug)]
pub struct WorkloadTarget {
    pub id: String,
    pub name: String,
}

impl WorkloadTree {
    pub fn containing_folder(&self, id: &str) -> Option<&str> {
        self.placements
            .iter()
            .find(|p| p.agent_id == id)
            .and_then(|p| p.folder_id.as_deref())
    }

    pub fn folder_path(&self, id: Option<&str>) -> Result<String, ProtocolError> {
        let mut names = Vec::new();
        let mut seen = HashSet::new();
        let mut next = id;
        while let Some(id) = next {
            if !seen.insert(id) {
                return Err(ProtocolError::new(
                    "invalid_workload_tree",
                    "folder cycle detected",
                ));
            }
            let folder = self
                .folders
                .iter()
                .find(|f| f.folder_id == id)
                .ok_or_else(|| {
                    ProtocolError::new("folder_not_found", "folder no longer exists; retry")
                })?;
            names.push(folder.name.as_str());
            next = folder.parent_id.as_deref();
        }
        names.reverse();
        Ok(format!("/{}", names.join("/")))
    }

    pub fn workload_path(&self, id: &str, name: &str) -> Result<String, ProtocolError> {
        Ok(format!(
            "{}/{}",
            self.folder_path(self.containing_folder(id))?
                .trim_end_matches('/'),
            name
        ))
    }

    pub fn path_components(
        &self,
        path: &str,
        cwd: Option<&str>,
    ) -> Result<Vec<String>, ProtocolError> {
        if path.trim().is_empty() {
            return Err(ProtocolError::new(
                "invalid_tree_path",
                "path cannot be empty",
            ));
        }
        let mut parts: Vec<String> = if path.starts_with('/') {
            Vec::new()
        } else {
            self.folder_path(cwd)?
                .split('/')
                .filter(|p| !p.is_empty())
                .map(str::to_owned)
                .collect()
        };
        for part in path.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                name if name.trim() != name || name.chars().any(char::is_control) => {
                    return Err(ProtocolError::new(
                        "invalid_tree_path",
                        "path components must have visible names without surrounding whitespace",
                    ))
                }
                name => parts.push(name.to_owned()),
            }
        }
        Ok(parts)
    }

    pub fn resolve_folder_path(
        &self,
        path: &str,
        cwd: Option<&str>,
    ) -> Result<Option<String>, ProtocolError> {
        if let Some(folder) = self.folders.iter().find(|f| f.folder_id == path) {
            return Ok(Some(folder.folder_id.clone()));
        }
        let mut parent = None;
        for name in self.path_components(path, cwd)? {
            let mut matches = self
                .folders
                .iter()
                .filter(|f| f.parent_id == parent && f.name == name);
            let folder = matches.next().ok_or_else(|| {
                ProtocolError::new("folder_not_found", format!("folder not found: {path}"))
            })?;
            if matches.next().is_some() {
                return Err(ProtocolError::new(
                    "folder_ambiguous",
                    "duplicate folder name; use its ID",
                ));
            }
            parent = Some(folder.folder_id.clone());
        }
        Ok(parent)
    }

    pub fn resolve_target(
        &self,
        rows: &[WorkloadTarget],
        caller: Option<&str>,
        target: &str,
    ) -> Result<String, ProtocolError> {
        if matches!(target, "self" | ".") {
            return caller.map(str::to_owned).ok_or_else(|| {
                ProtocolError::new(
                    "agent_identity_required",
                    "self requires an authenticated Agent identity",
                )
            });
        }
        if target.starts_with("ag_")
            || target.starts_with("app_")
            || rows.iter().any(|r| r.id == target)
        {
            return Ok(target.to_owned());
        }
        let cwd = caller.and_then(|id| self.containing_folder(id));
        let (folder, name) = if target.contains('/') {
            if target.ends_with('/') || matches!(target.rsplit('/').next(), Some("." | "..")) {
                return Err(ProtocolError::new(
                    "invalid_agent_target",
                    "a folder is not an Agent target",
                ));
            }
            let parts = self.path_components(target, cwd)?;
            let name = parts
                .last()
                .ok_or_else(|| {
                    ProtocolError::new("invalid_agent_target", "workspace root is not an Agent")
                })?
                .clone();
            let parent = self
                .resolve_folder_path(&format!("/{}", parts[..parts.len() - 1].join("/")), None)?;
            (parent, name)
        } else {
            (cwd.map(str::to_owned), target.to_owned())
        };
        let matches: Vec<_> = rows
            .iter()
            .filter(|r| r.name == name && self.containing_folder(&r.id) == folder.as_deref())
            .collect();
        match matches.as_slice() {
            [row] => Ok(row.id.clone()),
            [] if !target.contains('/') => {
                let mut paths = rows
                    .iter()
                    .filter(|r| r.name == target)
                    .map(|r| self.workload_path(&r.id, &r.name))
                    .collect::<Result<Vec<_>, _>>()?;
                paths.sort();
                paths.dedup();
                paths.truncate(10);
                let mut error = ProtocolError::new(
                    "agent_not_in_folder",
                    format!(
                        "no Agent named {target} in {}; use a tree path or stable ID",
                        self.folder_path(cwd)?
                    ),
                );
                error.hint = Some(paths);
                Err(error)
            }
            [] => Err(ProtocolError::new(
                "agent_not_found",
                format!("Agent not found at {target}"),
            )),
            _ => Err(ProtocolError::new(
                "agent_ambiguous",
                "multiple Agents in this folder have this name; use a stable ID",
            )),
        }
    }
}

/// New labels must remain unambiguous path components. Existing data is not renamed.
pub fn validate_workload_label(name: &str) -> Result<(), ProtocolError> {
    if name.is_empty()
        || name.trim() != name
        || matches!(name, "self" | "." | "..")
        || name.contains('/')
        || name.chars().any(char::is_control)
        || name.starts_with("ag_")
        || name.starts_with("app_")
    {
        return Err(ProtocolError::new(
            "invalid_agent_name",
            "Agent name must be a visible path component, not self, dot, or a stable ID",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{WorkloadFolder, WorkloadPlacement};
    #[test]
    fn folder_paths_are_never_workload_targets() {
        let tree = WorkloadTree {
            folders: vec![],
            placements: vec![],
        };
        let rows = vec![WorkloadTarget {
            id: "ag_a".into(),
            name: "a".into(),
        }];
        for path in ["/a/.", "/a/..", "./.", "/", "a/"] {
            assert_eq!(
                tree.resolve_target(&rows, None, path).unwrap_err().code,
                "invalid_agent_target"
            );
        }
    }

    #[test]
    fn paths_are_relative_to_callers_folder_and_bare_names_never_escape_it() {
        let tree = WorkloadTree {
            folders: vec![
                WorkloadFolder {
                    folder_id: "a".into(),
                    name: "a".into(),
                    parent_id: None,
                },
                WorkloadFolder {
                    folder_id: "b".into(),
                    name: "b".into(),
                    parent_id: None,
                },
                WorkloadFolder {
                    folder_id: "sub".into(),
                    name: "sub".into(),
                    parent_id: Some("a".into()),
                },
            ],
            placements: vec![
                ("ag_c", "a"),
                ("ag_a", "a"),
                ("ag_b", "b"),
                ("app_sub", "sub"),
            ]
            .into_iter()
            .map(|(id, f)| WorkloadPlacement {
                agent_id: id.into(),
                folder_id: Some(f.into()),
                position: 0,
            })
            .collect(),
        };
        let rows = vec![
            ("ag_a", "reviewer"),
            ("ag_b", "reviewer"),
            ("app_sub", "ui"),
        ]
        .into_iter()
        .map(|(id, n)| WorkloadTarget {
            id: id.into(),
            name: n.into(),
        })
        .collect::<Vec<_>>();
        for (input, expected) in [
            ("reviewer", "ag_a"),
            ("./reviewer", "ag_a"),
            ("/b/reviewer", "ag_b"),
            ("../b/reviewer", "ag_b"),
            ("sub/ui", "app_sub"),
            ("./sub/../reviewer", "ag_a"),
            ("ag_b", "ag_b"),
            ("app_sub", "app_sub"),
            ("self", "ag_c"),
            (".", "ag_c"),
        ] {
            assert_eq!(
                tree.resolve_target(&rows, Some("ag_c"), input).unwrap(),
                expected,
                "{input}"
            );
        }
        let error = tree.resolve_target(&rows, None, "reviewer").unwrap_err();
        assert_eq!(error.code, "agent_not_in_folder");
        assert_eq!(error.hint.unwrap(), ["/a/reviewer", "/b/reviewer"]);
        assert!(tree.resolve_target(&rows, None, "self").is_err());
        assert!(tree.resolve_target(&rows, None, "/a/").is_err());
        assert!(tree.resolve_target(&rows, None, "/").is_err());
        assert_eq!(
            tree.resolve_target(&rows, None, "../../a/reviewer")
                .unwrap(),
            "ag_a"
        );
    }
    #[test]
    fn hints_are_bounded_and_labels_cannot_shadow_target_syntax() {
        let tree = WorkloadTree {
            folders: (0..20)
                .map(|i| WorkloadFolder {
                    folder_id: format!("f{i}"),
                    name: format!("team{i}"),
                    parent_id: None,
                })
                .collect(),
            placements: (0..20)
                .map(|i| WorkloadPlacement {
                    agent_id: format!("ag_{i}"),
                    folder_id: Some(format!("f{i}")),
                    position: 0,
                })
                .collect(),
        };
        let rows = (0..20)
            .map(|i| WorkloadTarget {
                id: format!("ag_{i}"),
                name: "reviewer".into(),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            tree.resolve_target(&rows, None, "reviewer")
                .unwrap_err()
                .hint
                .unwrap()
                .len(),
            10
        );
        for name in ["self", ".", "..", "a/b", "ag_x", "app_x", " a"] {
            assert!(validate_workload_label(name).is_err());
        }
        assert!(validate_workload_label("reviewer").is_ok());
    }
}
