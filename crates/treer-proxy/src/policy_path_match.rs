use super::PolicyRequest;
use treer_protocol::{
    PolicyFolderField, PolicyFolderRef, PolicyTreeCondition, PolicyTreeContext, PolicyTreeSelector,
    WorkspacePolicyRule,
};

pub(super) fn selector_matches(
    selector: Option<&PolicyTreeSelector>,
    context: Option<&PolicyTreeContext>,
) -> bool {
    let Some(selector) = selector else {
        return true;
    };
    let Some(context) = context.filter(|c| c.placed) else {
        return false;
    };
    selector.kind.as_ref().is_none_or(|v| *v == context.kind)
        && selector.name.as_ref().is_none_or(|v| *v == context.name)
        && selector
            .folder_id
            .as_ref()
            .is_none_or(|v| context.folder_id.as_ref() == Some(v))
        && selector.path_eq.as_ref().is_none_or(|v| *v == context.path)
        && selector
            .path_glob
            .as_ref()
            .is_none_or(|v| glob(v, &context.path))
        && selector.path_prefix.as_ref().is_none_or(|v| {
            (v.include_self && v.path == context.path) || below(&v.path, &context.path)
        })
}

fn below(parent: &str, path: &str) -> bool {
    path != parent
        && (parent == "/"
            || path
                .strip_prefix(parent)
                .is_some_and(|s| s.starts_with('/')))
}

fn glob(pattern: &str, path: &str) -> bool {
    let parts: Vec<_> = pattern.split('/').filter(|p| !p.is_empty()).collect();
    let actual: Vec<_> = path.split('/').filter(|p| !p.is_empty()).collect();
    for (i, part) in parts.iter().enumerate() {
        if *part == "**" {
            return true;
        }
        if actual.get(i).is_none_or(|v| *part != "*" && v != part) {
            return false;
        }
    }
    parts.len() == actual.len()
}

pub(super) fn conditions_match(rule: &WorkspacePolicyRule, request: &PolicyRequest) -> bool {
    rule.conditions.iter().all(|condition| {
        let Some(target) = request.resource_tree.as_ref().filter(|c| c.placed) else {
            return false;
        };
        match condition {
            PolicyTreeCondition::SameFolder => request
                .subject_tree
                .as_ref()
                .is_some_and(|subject| subject.placed && subject.folder_id == target.folder_id),
            PolicyTreeCondition::ResourceBelowSubjectFolder { strict } => {
                request.subject_tree.as_ref().is_some_and(|subject| {
                    subject.placed
                        && below_context(
                            target,
                            subject.folder_id.as_deref(),
                            &subject.folder_path,
                            *strict,
                        )
                })
            }
            PolicyTreeCondition::ResourceBelowFolder { folder, strict } => {
                folder_matches(folder, rule, request, target, *strict)
            }
        }
    })
}

fn below_context(target: &PolicyTreeContext, id: Option<&str>, path: &str, strict: bool) -> bool {
    match id {
        Some(id) => {
            (target.ancestor_ids.iter().any(|a| a == id)
                || target.kind == "folder" && target.node_id == id)
                && (!strict
                    || target.folder_id.as_deref() != Some(id)
                        && !(target.kind == "folder" && target.node_id == id))
        }
        None => path == "/" && (!strict || target.folder_path != "/"),
    }
}

fn folder_matches(
    reference: &PolicyFolderRef,
    rule: &WorkspacePolicyRule,
    request: &PolicyRequest,
    target: &PolicyTreeContext,
    strict: bool,
) -> bool {
    match reference {
        PolicyFolderRef::Variable(v) => rule.bindings.get(&v.var).is_some_and(|value| {
            !matches!(value, PolicyFolderRef::Variable(_))
                && folder_matches(value, rule, request, target, strict)
        }),
        PolicyFolderRef::Field(v) => {
            let context = match v.field {
                PolicyFolderField::Subject => request.subject_tree.as_ref(),
                PolicyFolderField::Resource => request.resource_tree.as_ref(),
            };
            context.is_some_and(|c| {
                c.placed && below_context(target, c.folder_id.as_deref(), &c.folder_path, strict)
            })
        }
        PolicyFolderRef::Id(v) => below_context(target, Some(&v.id), "", strict),
        PolicyFolderRef::Path(v) => {
            let is_folder = target.kind == "folder" && target.path == v.path;
            (target.ancestor_paths.contains(&v.path) || is_folder)
                && (!strict || target.folder_path != v.path && !is_folder)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn glob_uses_component_boundaries_and_zero_length_terminal_globstar() {
        for (pattern, path, expected) in [
            ("/team/*", "/team/reviewer", true),
            ("/team/*", "/team", false),
            ("/team/*", "/team/sub/reviewer", false),
            ("/infra/**", "/infra", true),
            ("/infra/**", "/infra/sub/tool", true),
            ("/infra/**", "/infrastructure/tool", false),
            ("/", "/", true),
            ("/*", "/", false),
        ] {
            assert_eq!(glob(pattern, path), expected, "{pattern} {path}");
        }
    }
}
