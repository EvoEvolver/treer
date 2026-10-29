//! Rules with administration actions need owner-verified publication.
//!
//! A Provider bound with `provider_operator` lets any operator-token holder
//! publish, so the operator validator only accepts a document whose
//! administration rules cannot grant more than an owner last approved: every
//! allow rule naming an administration action must be one an owner validated
//! (same rule and same referenced groups), and every approved deny rule must
//! still be present. The owner validator records the approved set; when it
//! approves a new allow rule with an organization-scope action, the verifying
//! human must be an organization owner/admin.
use super::*;
use crate::policy::ADMIN_ACTIONS;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use treer_protocol::{PolicyEffect, WorkspacePolicyDocument, ORG_AGENT_GRANTABLE_ACTIONS};

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct ApprovedAllow {
    fingerprint: String,
    /// Approved by an organization owner/admin (organization-scope actions).
    org: bool,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct AdminRules {
    /// `(fingerprint, has an organization-scope action)`.
    allow: Vec<(String, bool)>,
    deny: BTreeSet<String>,
}

pub(super) fn admin_rules(document: &WorkspacePolicyDocument) -> AdminRules {
    let mut rules = AdminRules::default();
    for rule in &document.rules {
        if !rule
            .actions
            .iter()
            .any(|action| ADMIN_ACTIONS.contains(&action.as_str()))
        {
            continue;
        }
        let groups: BTreeMap<&str, _> = rule
            .subjects
            .iter()
            .filter_map(|selector| selector.group.as_deref())
            .chain(
                rule.resources
                    .iter()
                    .filter_map(|selector| selector.principal_group.as_deref()),
            )
            .map(|name| (name, document.groups.get(name)))
            .collect();
        let canonical =
            serde_json::to_vec(&json!({ "rule": rule, "groups": groups })).unwrap_or_default();
        let fingerprint = format!("{:x}", Sha256::digest(canonical));
        match rule.effect {
            PolicyEffect::Allow => rules.allow.push((
                fingerprint,
                rule.actions
                    .iter()
                    .any(|action| ORG_AGENT_GRANTABLE_ACTIONS.contains(&action.as_str())),
            )),
            PolicyEffect::Deny => {
                rules.deny.insert(fingerprint);
            }
        }
    }
    rules
}

#[derive(Debug, Default)]
pub(super) struct Approval {
    allow: Vec<ApprovedAllow>,
    deny: BTreeSet<String>,
}

fn store_error(_: sqlx::Error) -> ApiFailure {
    ApiFailure::internal(
        "policy_admin_approval_failed",
        "cannot read or record the administration rule approval",
    )
}

pub(super) async fn approval(auth: &AuthStore, workspace: &str) -> Result<Approval, ApiFailure> {
    let row = sqlx::query(
        "SELECT allow_rules, deny_rules FROM workspace_policy_admin_approvals WHERE workspace_id = $1",
    )
    .bind(workspace)
    .fetch_optional(&auth.pool())
    .await
    .map_err(store_error)?;
    use sqlx::Row;
    Ok(row
        .map(|row| Approval {
            allow: serde_json::from_value(row.get("allow_rules")).unwrap_or_default(),
            deny: serde_json::from_value(row.get("deny_rules")).unwrap_or_default(),
        })
        .unwrap_or_default())
}

fn owner_session_required() -> ApiFailure {
    ApiFailure::forbidden(
        "policy_admin_owner_session_required",
        "rules with administration actions changed; publish them with a verified workspace owner (owner_session), even under provider_operator",
    )
}

/// The operator (`provider_operator`) gate: the candidate's administration
/// rules must not grant beyond the owner-approved set.
pub(super) async fn require_approved(
    auth: &AuthStore,
    workspace: &str,
    document: &WorkspacePolicyDocument,
) -> Result<(), ApiFailure> {
    let candidate = admin_rules(document);
    let approved = approval(auth, workspace).await?;
    let allows_approved = candidate.allow.iter().all(|(fingerprint, _)| {
        approved
            .allow
            .iter()
            .any(|entry| &entry.fingerprint == fingerprint)
    });
    if allows_approved && approved.deny.is_subset(&candidate.deny) {
        Ok(())
    } else {
        Err(owner_session_required())
    }
}

/// The owner validator: record the candidate's administration rules as
/// approved by `user_id`. A new allow rule with an organization-scope action
/// needs an organization owner/admin.
pub(super) async fn record_owner_approval(
    auth: &AuthStore,
    workspace: &str,
    user_id: &str,
    document: &WorkspacePolicyDocument,
) -> Result<(), ApiFailure> {
    let candidate = admin_rules(document);
    let previous = approval(auth, workspace).await?;
    if candidate.allow.is_empty() && candidate.deny.is_empty() && previous.allow.is_empty() {
        return Ok(());
    }
    let organization = auth.workspace_organization(workspace).await?;
    let manager = auth
        .membership_role(&organization, user_id)
        .await?
        .is_some_and(|role| matches!(role.as_str(), "owner" | "admin"));
    let mut allow = Vec::new();
    for (fingerprint, org_scope) in &candidate.allow {
        let previously_org = previous
            .allow
            .iter()
            .any(|entry| &entry.fingerprint == fingerprint && entry.org);
        if *org_scope && !manager && !previously_org {
            return Err(ApiFailure::forbidden(
                "policy_org_admin_required",
                "rules granting organization administration actions must be published by an organization owner or admin",
            ));
        }
        allow.push(ApprovedAllow {
            fingerprint: fingerprint.clone(),
            org: manager || previously_org,
        });
    }
    sqlx::query(
        "INSERT INTO workspace_policy_admin_approvals(workspace_id, allow_rules, deny_rules, \
         approved_by, approved_at) VALUES($1, $2, $3, $4, now()) ON CONFLICT(workspace_id) DO UPDATE \
         SET allow_rules = EXCLUDED.allow_rules, deny_rules = EXCLUDED.deny_rules, \
         approved_by = EXCLUDED.approved_by, approved_at = EXCLUDED.approved_at",
    )
    .bind(workspace)
    .bind(json!(allow))
    .bind(json!(candidate.deny))
    .bind(user_id)
    .execute(&auth.pool())
    .await
    .map_err(store_error)?;
    Ok(())
}
