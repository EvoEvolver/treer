//! Audit chart categories (#187, #192): one table from recorded audit action
//! names to the eight fixed categories the Console charts use. The Proxy
//! applies it at read time for the summary API and the list route's
//! `category` filter; the Console keeps only labels keyed by the wire names.
//! Categorization is presentation, not authorization: it never widens or
//! narrows what a caller may read. Matching is exact name first, then the
//! longest registered prefix (a prefix matches `prefix.` + anything), else
//! [`AuditCategory::Other`], which stays visible so an unmapped action is
//! noticed rather than hidden. Action names are server constants; no request
//! field is ever recorded as an action.
use serde::{Deserialize, Serialize};

/// Incremented whenever the table changes; returned by the summary and list
/// routes so the Console can say which mapping a chart was drawn with.
pub const AUDIT_CATEGORY_TABLE_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditCategory {
    AgentLifecycle,
    Messages,
    Tree,
    Policy,
    AppsIngress,
    MachinesFiles,
    AccessAdmin,
    Other,
}

impl AuditCategory {
    /// Fixed stack order for charts; `Other` is always last.
    pub const ALL: [AuditCategory; 8] = [
        AuditCategory::AgentLifecycle,
        AuditCategory::Messages,
        AuditCategory::Tree,
        AuditCategory::Policy,
        AuditCategory::AppsIngress,
        AuditCategory::MachinesFiles,
        AuditCategory::AccessAdmin,
        AuditCategory::Other,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            AuditCategory::AgentLifecycle => "agent_lifecycle",
            AuditCategory::Messages => "messages",
            AuditCategory::Tree => "tree",
            AuditCategory::Policy => "policy",
            AuditCategory::AppsIngress => "apps_ingress",
            AuditCategory::MachinesFiles => "machines_files",
            AuditCategory::AccessAdmin => "access_admin",
            AuditCategory::Other => "other",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == value)
    }
}

impl std::fmt::Display for AuditCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Exact action names. Every literal a Proxy recorder writes must appear here
/// or under a prefix below; the Proxy's scan test enforces that.
pub const AUDIT_CATEGORY_EXACT: &[(&str, AuditCategory)] = &[
    // Agent lifecycle (includes launch profiles and the launcher's session end).
    ("agent.created", AuditCategory::AgentLifecycle),
    ("agent.create.failed", AuditCategory::AgentLifecycle),
    ("agent.deleted", AuditCategory::AgentLifecycle),
    ("agent.started", AuditCategory::AgentLifecycle),
    ("agent.stopped", AuditCategory::AgentLifecycle),
    ("agent.restarted", AuditCategory::AgentLifecycle),
    ("agent.aborted", AuditCategory::AgentLifecycle),
    ("agent.renamed", AuditCategory::AgentLifecycle),
    ("agent.updated", AuditCategory::AgentLifecycle),
    ("agent.startup.set", AuditCategory::AgentLifecycle),
    ("agent.startup.cleared", AuditCategory::AgentLifecycle),
    ("agent.startup.restarted", AuditCategory::AgentLifecycle),
    ("agent.startup.admin_set", AuditCategory::AgentLifecycle),
    ("agent.reservation.cleanup", AuditCategory::AgentLifecycle),
    (
        "agent.interface.origin.create",
        AuditCategory::AgentLifecycle,
    ),
    ("server.updated", AuditCategory::AgentLifecycle),
    ("session.finished", AuditCategory::AgentLifecycle),
    // Embed handshake mints (#198): a prewarm is a hover, never an access.
    (
        "agent.interface.embed.minted",
        AuditCategory::AgentLifecycle,
    ),
    (
        "agent.interface.embed.prewarmed",
        AuditCategory::AgentLifecycle,
    ),
    // Messages and prompts.
    ("message.created", AuditCategory::Messages),
    ("message.imported", AuditCategory::Messages),
    ("message.acknowledged", AuditCategory::Messages),
    ("agent.prompted", AuditCategory::Messages),
    ("email.send", AuditCategory::Messages),
    // Workspace structure.
    ("tree.workload.rename", AuditCategory::Tree),
    ("workspace.pins.changed", AuditCategory::Tree),
    // Apps and ingress.
    ("app.created", AuditCategory::AppsIngress),
    ("app.deleted", AuditCategory::AppsIngress),
    ("app.started", AuditCategory::AppsIngress),
    ("app.stopped", AuditCategory::AppsIngress),
    ("app.restarted", AuditCategory::AppsIngress),
    ("app.startup.updated", AuditCategory::AppsIngress),
    ("app.access.updated", AuditCategory::AppsIngress),
    // Machines and files.
    ("machine.enroll", AuditCategory::MachinesFiles),
    ("machine.reenroll", AuditCategory::MachinesFiles),
    ("machine.renamed", AuditCategory::MachinesFiles),
    ("machine.deleted", AuditCategory::MachinesFiles),
    ("machine.update", AuditCategory::MachinesFiles),
    ("machine.exec", AuditCategory::MachinesFiles),
    ("machine.upload", AuditCategory::MachinesFiles),
    // Access and administration.
    ("member.role_updated", AuditCategory::AccessAdmin),
    ("member.removed", AuditCategory::AccessAdmin),
    ("invitation.created", AuditCategory::AccessAdmin),
    ("invitation.pending", AuditCategory::AccessAdmin),
    ("invitation.accepted", AuditCategory::AccessAdmin),
    ("invitation.revoked", AuditCategory::AccessAdmin),
    ("organization.created", AuditCategory::AccessAdmin),
    ("organization.renamed", AuditCategory::AccessAdmin),
    ("organization.invite", AuditCategory::AccessAdmin),
    ("workspace.created", AuditCategory::AccessAdmin),
    ("workspace.renamed", AuditCategory::AccessAdmin),
    ("workspace.deleted", AuditCategory::AccessAdmin),
    ("identity.token.issue", AuditCategory::AccessAdmin),
];

/// Prefixes, matched as `prefix.` followed by anything, longest prefix wins.
pub const AUDIT_CATEGORY_PREFIXES: &[(&str, AuditCategory)] = &[
    ("launch_profile", AuditCategory::AgentLifecycle),
    ("message", AuditCategory::Messages),
    ("tree", AuditCategory::Tree),
    ("policy", AuditCategory::Policy),
    ("policy_provider", AuditCategory::Policy),
    ("service", AuditCategory::AppsIngress),
    ("ingress", AuditCategory::AppsIngress),
    ("virtual_host", AuditCategory::AppsIngress),
    ("network", AuditCategory::AppsIngress),
    ("app", AuditCategory::AppsIngress),
    ("machine.file", AuditCategory::MachinesFiles),
    ("provider_cli", AuditCategory::MachinesFiles),
    ("member", AuditCategory::AccessAdmin),
    ("invitation", AuditCategory::AccessAdmin),
    ("session", AuditCategory::AccessAdmin),
    ("auth", AuditCategory::AccessAdmin),
    ("identity", AuditCategory::AccessAdmin),
];

/// Exact first, then the longest prefix, else `Other`.
pub fn audit_category(action: &str) -> AuditCategory {
    if let Some((_, category)) = AUDIT_CATEGORY_EXACT
        .iter()
        .find(|(name, _)| *name == action)
    {
        return *category;
    }
    AUDIT_CATEGORY_PREFIXES
        .iter()
        .filter(|(prefix, _)| {
            action.len() > prefix.len()
                && action.as_bytes()[prefix.len()] == b'.'
                && action.starts_with(prefix)
        })
        .max_by_key(|(prefix, _)| prefix.len())
        .map(|(_, category)| *category)
        .unwrap_or(AuditCategory::Other)
}

/// The exact names and `prefix.`-forms of one category, for binding into SQL
/// (`action = ANY($exact) OR left(action, length(p)) = p` over the prefixes).
/// Prefix terms carry the trailing dot so the SQL test cannot match a sibling
/// such as `policyX.y` or `agent_created`.
pub fn audit_category_terms(category: AuditCategory) -> (Vec<&'static str>, Vec<String>) {
    let exact = AUDIT_CATEGORY_EXACT
        .iter()
        .filter(|(_, c)| *c == category)
        .map(|(name, _)| *name)
        .collect();
    let prefixes = AUDIT_CATEGORY_PREFIXES
        .iter()
        .filter(|(_, c)| *c == category)
        .map(|(prefix, _)| format!("{prefix}."))
        .collect();
    (exact, prefixes)
}

/// Notable events for the annotated timeline (#191): always notable actions,
/// plus actions notable only when they failed, plus every failed event in
/// `Other` or `AccessAdmin` (a new failing action is seen; failed logins in
/// the workspace scope are seen).
pub const NOTABLE_ACTIONS: &[&str] = &[
    "member.role_updated",
    "member.removed",
    "invitation.created",
    "invitation.accepted",
    "invitation.revoked",
    "agent.created",
    "agent.create.failed",
    "agent.deleted",
    "agent.startup.admin_set",
    "app.created",
    "app.deleted",
    "ingress.create",
    "ingress.delete",
    "ingress.update",
    "ingress.limits.update",
    "identity.token.issue",
    "machine.enroll",
    "machine.reenroll",
    "machine.deleted",
    "provider_cli.install.finished",
    "workspace.deleted",
    "workspace.renamed",
];

/// Prefixes whose every action is notable (`prefix.`-form matching).
pub const NOTABLE_PREFIXES: &[&str] = &[
    "policy",
    "policy_provider",
    "ingress.http",
    "ingress.tcp",
    "launch_profile",
];

/// Actions notable only with outcome `failed` (routine when they succeed).
pub const NOTABLE_WHEN_FAILED: &[&str] = &["machine.exec", "machine.file.write"];

pub fn is_notable(action: &str, failed: bool) -> bool {
    if NOTABLE_ACTIONS.contains(&action) {
        return true;
    }
    if NOTABLE_PREFIXES.iter().any(|p| {
        action.len() > p.len() && action.starts_with(p) && action.as_bytes()[p.len()] == b'.'
    }) {
        return true;
    }
    if failed {
        if NOTABLE_WHEN_FAILED.contains(&action) {
            return true;
        }
        return matches!(
            audit_category(action),
            AuditCategory::Other | AuditCategory::AccessAdmin
        );
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_wins_then_longest_prefix_then_other() {
        assert_eq!(audit_category("agent.prompted"), AuditCategory::Messages);
        assert_eq!(
            audit_category("agent.created"),
            AuditCategory::AgentLifecycle
        );
        assert_eq!(
            audit_category("policy_provider.configured"),
            AuditCategory::Policy
        );
        assert_eq!(audit_category("policy.publish"), AuditCategory::Policy);
        assert_eq!(
            audit_category("machine.file.uploaded"),
            AuditCategory::MachinesFiles
        );
        assert_eq!(
            audit_category("app.something.new"),
            AuditCategory::AppsIngress
        );
        assert_eq!(
            audit_category("session.finished"),
            AuditCategory::AgentLifecycle
        );
        assert_eq!(audit_category("session.update"), AuditCategory::AccessAdmin);
        assert_eq!(
            audit_category("workspace.renamed"),
            AuditCategory::AccessAdmin
        );
        assert_eq!(
            audit_category("workspace.pins.changed"),
            AuditCategory::Tree
        );
        // Near misses never match a prefix: a different separator, a longer
        // first label, or a prefix longer than the action.
        for miss in [
            "policyXprovider.x",
            "agent_created",
            "policy_providerx.y",
            "policyx.y",
            "policy",
            "app",
            "agentx.created",
            "",
        ] {
            assert_eq!(audit_category(miss), AuditCategory::Other, "{miss}");
        }
        // Unknown actions are visible as other, never dropped.
        assert_eq!(audit_category("brand.new.action"), AuditCategory::Other);
    }

    #[test]
    fn terms_carry_the_dot_and_wire_names_are_stable() {
        let (exact, prefixes) = audit_category_terms(AuditCategory::Policy);
        assert!(exact.is_empty());
        assert_eq!(
            prefixes,
            vec!["policy.".to_string(), "policy_provider.".to_string()]
        );
        assert_eq!(AuditCategory::ALL.last(), Some(&AuditCategory::Other));
        for category in AuditCategory::ALL {
            assert_eq!(AuditCategory::parse(category.as_str()), Some(category));
            assert_eq!(
                serde_json::to_string(&category).unwrap(),
                format!("\"{}\"", category.as_str())
            );
        }
        assert_eq!(AUDIT_CATEGORY_TABLE_VERSION, 1);
    }

    #[test]
    fn notable_rules() {
        assert!(is_notable("member.removed", false));
        assert!(is_notable("policy_provider.removed", false));
        assert!(is_notable("ingress.http.create", false));
        assert!(!is_notable("machine.exec", false));
        assert!(is_notable("machine.exec", true));
        assert!(!is_notable("machine.file.write", false));
        assert!(is_notable("machine.file.write", true));
        assert!(!is_notable("message.created", true));
        assert!(
            is_notable("brand.new.action", true),
            "a failed unmapped action is seen"
        );
        assert!(!is_notable("brand.new.action", false));
        assert!(
            is_notable("session.update", true),
            "failed auth-class events are seen"
        );
        assert!(!is_notable("machine.file.read", true));
    }
}
