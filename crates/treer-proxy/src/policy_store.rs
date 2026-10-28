use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::postgres::PgRow;
use sqlx::{PgPool, Row};
use treer_protocol::{
    PolicyMode, PolicyPrincipalKind, PolicyPrincipalRef, WorkspacePolicy, WorkspacePolicyDocument,
    POLICY_SCHEMA_VERSION, POLICY_SCHEMA_VERSION_V2,
};

pub const POLICY_CHANGED_CHANNEL: &str = "treer_policy_changed";
pub const MAX_POLICY_DOCUMENT_BYTES: usize = 256 * 1024;
pub const MAX_POLICY_RULES: usize = 1_000;
#[cfg(test)]
#[path = "policy_path_validation_tests.rs"]
mod path_validation_tests;

#[derive(Clone)]
pub struct WorkspacePolicyStore {
    pool: PgPool,
}

impl WorkspacePolicyStore {
    pub fn pool(&self) -> PgPool {
        self.pool.clone()
    }
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn get(
        &self,
        workspace_id: &str,
    ) -> Result<Option<WorkspacePolicy>, PolicyStoreError> {
        let row = sqlx::query(
            "SELECT workspace_id, revision, schema_version, mode, document::text AS document, updated_at, \
                    updated_by_kind, updated_by_id \
             FROM workspace_policies WHERE workspace_id = $1",
        )
        .bind(workspace_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(workspace_policy_from_row).transpose()
    }

    pub async fn replace(
        &self,
        workspace_id: &str,
        expected_revision: u64,
        mode: PolicyMode,
        document: WorkspacePolicyDocument,
        updated_by: PolicyPrincipalRef,
    ) -> Result<WorkspacePolicy, PolicyStoreError> {
        validate_document(&document)?;
        validate_token(&updated_by.id, "policy actor ID")?;
        let document_json = serde_json::to_string(&document)
            .map_err(|error| PolicyStoreError::Corrupt(error.to_string()))?;
        let expected_revision = i64::try_from(expected_revision).map_err(|_| {
            PolicyStoreError::invalid("invalid_policy_revision", "policy revision is too large")
        })?;
        let now = Utc::now();
        let mut transaction = self.pool.begin().await?;
        let row = if expected_revision == 0 {
            sqlx::query(
                "INSERT INTO workspace_policies(\
                    workspace_id, revision, schema_version, mode, document, updated_at, \
                    updated_by_kind, updated_by_id\
                 ) VALUES($1, 1, $2, $3, $4::jsonb, $5, $6, $7) \
                 ON CONFLICT(workspace_id) DO NOTHING \
                 RETURNING workspace_id, revision, schema_version, mode, document::text AS document, updated_at, \
                           updated_by_kind, updated_by_id",
            )
            .bind(workspace_id)
            .bind(i64::from(document.schema_version))
            .bind(mode.as_str())
            .bind(&document_json)
            .bind(now.to_rfc3339())
            .bind(updated_by.kind.as_str())
            .bind(&updated_by.id)
            .fetch_optional(&mut *transaction)
            .await?
        } else {
            sqlx::query(
                "UPDATE workspace_policies SET \
                    revision = revision + 1, schema_version = $3, mode = $4, document = $5::jsonb, \
                    updated_at = $6, updated_by_kind = $7, updated_by_id = $8 \
                 WHERE workspace_id = $1 AND revision = $2 \
                 RETURNING workspace_id, revision, schema_version, mode, document::text AS document, updated_at, \
                           updated_by_kind, updated_by_id",
            )
            .bind(workspace_id)
            .bind(expected_revision)
            .bind(i64::from(document.schema_version))
            .bind(mode.as_str())
            .bind(&document_json)
            .bind(now.to_rfc3339())
            .bind(updated_by.kind.as_str())
            .bind(&updated_by.id)
            .fetch_optional(&mut *transaction)
            .await?
        }
        .ok_or(PolicyStoreError::RevisionConflict)?;
        let policy = workspace_policy_from_row(row)?;
        let notification = json!({
            "workspace_id": workspace_id,
            "revision": policy.revision,
        })
        .to_string();
        sqlx::query("SELECT pg_notify($1, $2)")
            .bind(POLICY_CHANGED_CHANNEL)
            .bind(notification)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(policy)
    }
}

pub fn validate_document(document: &WorkspacePolicyDocument) -> Result<(), PolicyStoreError> {
    if !matches!(
        document.schema_version,
        POLICY_SCHEMA_VERSION | POLICY_SCHEMA_VERSION_V2
    ) {
        return Err(PolicyStoreError::invalid(
            "unsupported_policy_schema",
            "workspace policy schema version is not supported",
        ));
    }
    let encoded = serde_json::to_vec(document)
        .map_err(|error| PolicyStoreError::Corrupt(error.to_string()))?;
    if encoded.len() > MAX_POLICY_DOCUMENT_BYTES {
        return Err(PolicyStoreError::invalid(
            "policy_document_too_large",
            "workspace policy document exceeds 256 KiB",
        ));
    }
    if document.rules.len() > MAX_POLICY_RULES {
        return Err(PolicyStoreError::invalid(
            "too_many_policy_rules",
            "workspace policy may contain at most 1000 rules",
        ));
    }
    for action in document.defaults.keys() {
        validate_token(action, "policy action")?;
    }
    for (name, group) in &document.groups {
        validate_token(name, "policy group")?;
        for principal in &group.principals {
            validate_token(&principal.id, "policy principal ID")?;
        }
    }
    let mut rule_ids = HashSet::new();
    for rule in &document.rules {
        validate_path_rule(document.schema_version, rule)?;
        validate_token(&rule.id, "policy rule ID")?;
        if !rule_ids.insert(&rule.id) {
            return Err(PolicyStoreError::invalid(
                "duplicate_policy_rule",
                "workspace policy rule IDs must be unique",
            ));
        }
        if rule.subjects.is_empty() || rule.actions.is_empty() || rule.resources.is_empty() {
            return Err(PolicyStoreError::invalid(
                "invalid_policy_rule",
                "policy rules require subjects, actions, and resources",
            ));
        }
        for subject in &rule.subjects {
            if let Some(id) = &subject.id {
                validate_token(id, "policy subject ID")?;
            }
            if let Some(machine_id) = &subject.machine_id {
                validate_token(machine_id, "policy machine ID")?;
            }
            if let Some(group) = &subject.group {
                require_group(document, group, "subject")?;
            }
        }
        for action in &rule.actions {
            validate_token(action, "policy action")?;
        }
        for resource in &rule.resources {
            if let Some(kind) = &resource.kind {
                validate_token(kind, "policy resource kind")?;
            }
            if let Some(id) = &resource.id {
                validate_token(id, "policy resource ID")?;
            }
            if let Some(group) = &resource.principal_group {
                require_group(document, group, "resource")?;
            }
        }
    }
    Ok(())
}

fn validate_path_rule(
    schema: u32,
    rule: &treer_protocol::WorkspacePolicyRule,
) -> Result<(), PolicyStoreError> {
    use treer_protocol::{PolicyFolderRef, PolicyTreeCondition};
    let has_tree = rule.subjects.iter().any(|s| s.tree.is_some())
        || rule.resources.iter().any(|s| s.tree.is_some())
        || !rule.bindings.is_empty()
        || !rule.conditions.is_empty();
    if schema == POLICY_SCHEMA_VERSION && has_tree {
        return Err(PolicyStoreError::invalid(
            "unsupported_policy_schema",
            "Tree conditions require policy schema 2",
        ));
    }
    if rule.bindings.len() > 8 || rule.conditions.len() > 32 {
        return Err(PolicyStoreError::invalid(
            "policy_expression_too_large",
            "policy rule permits at most 8 bindings and 32 conditions",
        ));
    }
    let validate_ref =
        |reference: &PolicyFolderRef, binding: bool| -> Result<(), PolicyStoreError> {
            match reference {
                PolicyFolderRef::Variable(v) if binding || !rule.bindings.contains_key(&v.var) => {
                    Err(PolicyStoreError::invalid(
                        "invalid_policy_binding",
                        "binding chains or undeclared variables are not allowed",
                    ))
                }
                PolicyFolderRef::Variable(_) | PolicyFolderRef::Field(_) => Ok(()),
                PolicyFolderRef::Id(v) => validate_token(&v.id, "folder ID"),
                PolicyFolderRef::Path(v) => validate_policy_path(&v.path, false),
            }
        };
    for (name, value) in &rule.bindings {
        validate_token(name, "binding name")?;
        validate_ref(value, true)?;
    }
    for selector in rule
        .subjects
        .iter()
        .filter_map(|s| s.tree.as_ref())
        .chain(rule.resources.iter().filter_map(|s| s.tree.as_ref()))
    {
        if selector
            .kind
            .as_deref()
            .is_some_and(|k| !matches!(k, "agent" | "app" | "folder"))
        {
            return Err(PolicyStoreError::invalid(
                "invalid_policy_tree_kind",
                "Tree kind must be agent, app or folder",
            ));
        }
        if let Some(name) = &selector.name {
            validate_token(name, "Tree name")?;
        }
        if let Some(id) = &selector.folder_id {
            validate_token(id, "folder ID")?;
        }
        if usize::from(selector.path_eq.is_some())
            + usize::from(selector.path_glob.is_some())
            + usize::from(selector.path_prefix.is_some())
            > 1
        {
            return Err(PolicyStoreError::invalid(
                "invalid_policy_path",
                "choose one path comparator",
            ));
        }
        if let Some(path) = &selector.path_eq {
            validate_policy_path(path, false)?;
        }
        if let Some(path) = &selector.path_glob {
            validate_policy_path(path, true)?;
        }
        if let Some(prefix) = &selector.path_prefix {
            validate_policy_path(&prefix.path, false)?;
        }
    }
    for condition in &rule.conditions {
        if let PolicyTreeCondition::ResourceBelowFolder { folder, .. } = condition {
            validate_ref(folder, false)?;
        }
    }
    Ok(())
}

fn validate_policy_path(path: &str, glob: bool) -> Result<(), PolicyStoreError> {
    let parts: Vec<_> = path.strip_prefix('/').unwrap_or(path).split('/').collect();
    let valid = path.starts_with('/')
        && path.len() <= 4096
        && parts.len() <= 128
        && !path.chars().any(char::is_control)
        && (path == "/"
            || parts.iter().enumerate().all(|(i, p)| {
                !p.is_empty()
                    && p.trim() == *p
                    && !matches!(*p, "." | "..")
                    && (!glob
                        || (!p.contains(['?', '[', ']', '\\'])
                            && (!p.contains('*')
                                || *p == "*"
                                || (*p == "**" && i == parts.len() - 1))))
            }));
    if valid {
        Ok(())
    } else {
        Err(PolicyStoreError::invalid("invalid_policy_path", "path must be bounded, absolute and canonical; glob uses whole components and one terminal **"))
    }
}

fn require_group(
    document: &WorkspacePolicyDocument,
    group: &str,
    selector: &str,
) -> Result<(), PolicyStoreError> {
    if document.groups.contains_key(group) {
        Ok(())
    } else {
        Err(PolicyStoreError::invalid(
            "unknown_policy_group",
            format!("policy {selector} references unknown group {group}"),
        ))
    }
}

fn validate_token(value: &str, field: &str) -> Result<(), PolicyStoreError> {
    if value.is_empty()
        || value.len() > 128
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(PolicyStoreError::invalid(
            "invalid_policy_document",
            format!("{field} must contain 1-128 visible characters without surrounding space"),
        ));
    }
    Ok(())
}

fn workspace_policy_from_row(row: PgRow) -> Result<WorkspacePolicy, PolicyStoreError> {
    let revision = u64::try_from(row.get::<i64, _>("revision"))
        .map_err(|error| PolicyStoreError::Corrupt(format!("invalid revision: {error}")))?;
    let schema_version = u32::try_from(row.get::<i64, _>("schema_version"))
        .map_err(|error| PolicyStoreError::Corrupt(format!("invalid schema version: {error}")))?;
    let mode = match row.get::<String, _>("mode").as_str() {
        "monitor" => PolicyMode::Monitor,
        "enforce" => PolicyMode::Enforce,
        value => return Err(PolicyStoreError::Corrupt(format!("invalid mode {value}"))),
    };
    let document =
        serde_json::from_str::<WorkspacePolicyDocument>(&row.get::<String, _>("document"))
            .map_err(|error| PolicyStoreError::Corrupt(format!("invalid document: {error}")))?;
    if document.schema_version != schema_version {
        return Err(PolicyStoreError::Corrupt(
            "document and row schema versions disagree".to_string(),
        ));
    }
    let updated_by_kind = match row.get::<String, _>("updated_by_kind").as_str() {
        "human" => PolicyPrincipalKind::Human,
        "agent" => PolicyPrincipalKind::Agent,
        "machine" => PolicyPrincipalKind::Machine,
        "service" => PolicyPrincipalKind::Service,
        value => {
            return Err(PolicyStoreError::Corrupt(format!(
                "invalid actor kind {value}"
            )))
        }
    };
    let updated_at = row.get::<String, _>("updated_at");
    let updated_at = DateTime::parse_from_rfc3339(&updated_at)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| PolicyStoreError::Corrupt(format!("invalid timestamp: {error}")))?;
    Ok(WorkspacePolicy {
        workspace_id: row.get("workspace_id"),
        revision,
        mode,
        document,
        updated_at,
        updated_by: PolicyPrincipalRef {
            kind: updated_by_kind,
            id: row.get("updated_by_id"),
        },
    })
}

#[derive(Debug, thiserror::Error)]
pub enum PolicyStoreError {
    #[error("{code}: {message}")]
    Invalid { code: &'static str, message: String },
    #[error("policy_revision_conflict: workspace policy changed; reload it and retry")]
    RevisionConflict,
    #[error("policy database operation failed: {0}")]
    Database(#[from] sqlx::Error),
    #[error("stored policy is invalid: {0}")]
    Corrupt(String),
}

impl PolicyStoreError {
    fn invalid(code: &'static str, message: impl Into<String>) -> Self {
        Self::Invalid {
            code,
            message: message.into(),
        }
    }

    pub const fn code(&self) -> &'static str {
        match self {
            Self::Invalid { code, .. } => code,
            Self::RevisionConflict => "policy_revision_conflict",
            Self::Database(_) => "database_error",
            Self::Corrupt(_) => "invalid_stored_policy",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::PgPoolOptions;
    use uuid::Uuid;

    fn document() -> WorkspacePolicyDocument {
        serde_json::from_value(serde_json::json!({
            "schema_version": POLICY_SCHEMA_VERSION,
            "defaults": {"agent.prompt": "deny"},
            "rules": [{"id": "allow-self-read", "priority": 100, "effect": "allow",
                "subjects": [{"kind": "agent", "self": true}],
                "actions": ["agent.metadata.read"],
                "resources": [{"kind": "agent"}]}]
        }))
        .unwrap()
    }

    #[test]
    fn validation_accepts_bounded_documents_and_rejects_unknown_groups_and_duplicates() {
        validate_document(&document()).unwrap();
        let mut unknown_group = document();
        unknown_group.rules[0].resources[0].principal_group = Some("missing".to_string());
        assert_eq!(
            validate_document(&unknown_group).unwrap_err().code(),
            "unknown_policy_group"
        );
        let mut duplicate = document();
        duplicate.rules.push(duplicate.rules[0].clone());
        assert_eq!(
            validate_document(&duplicate).unwrap_err().code(),
            "duplicate_policy_rule"
        );
    }

    #[tokio::test]
    async fn jsonb_store_uses_optimistic_revisions() {
        let database_url = std::env::var("TREER_TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://treer:treer@127.0.0.1:55432/treer_test".to_string());
        let schema = format!("test_{}", Uuid::new_v4().simple());
        let setup_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .expect("connect to test PostgreSQL; start the documented Docker test database");
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&setup_pool)
            .await
            .expect("create isolated policy schema");
        setup_pool.close().await;

        let search_path = format!("SET search_path TO {schema}");
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .after_connect(move |connection, _| {
                let search_path = search_path.clone();
                Box::pin(async move {
                    sqlx::query(&search_path).execute(connection).await?;
                    Ok(())
                })
            })
            .connect(&database_url)
            .await
            .expect("connect to isolated policy schema");
        let mut transaction = pool.begin().await.expect("begin schema initialization");
        sqlx::raw_sql(include_str!("schema.sql"))
            .execute(&mut *transaction)
            .await
            .expect("initialize policy schema");
        transaction.commit().await.expect("commit policy schema");
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO organizations(organization_id, name, created_at, created_by) \
             VALUES('org_policy', 'Policy', $1, 'test')",
        )
        .bind(&now)
        .execute(&pool)
        .await
        .expect("seed policy organization");
        sqlx::query(
            "INSERT INTO workspaces(workspace_id, organization_id, name, created_at, created_by) \
             VALUES('policy', 'org_policy', 'Policy', $1, 'test')",
        )
        .bind(&now)
        .execute(&pool)
        .await
        .expect("seed policy workspace");

        let store = WorkspacePolicyStore::new(pool);
        assert!(store
            .get("policy")
            .await
            .expect("empty policy lookup")
            .is_none());
        let actor = PolicyPrincipalRef {
            kind: PolicyPrincipalKind::Human,
            id: "usr_owner".to_string(),
        };
        let created = store
            .replace("policy", 0, PolicyMode::Monitor, document(), actor.clone())
            .await
            .expect("create policy document");
        assert_eq!(created.revision, 1);
        assert_eq!(
            store
                .get("policy")
                .await
                .expect("read policy")
                .expect("stored policy"),
            created
        );
        assert_eq!(
            store
                .replace("policy", 0, PolicyMode::Enforce, document(), actor.clone())
                .await
                .expect_err("stale create revision")
                .code(),
            "policy_revision_conflict"
        );
        let updated = store
            .replace("policy", 1, PolicyMode::Enforce, document(), actor)
            .await
            .expect("update policy document");
        assert_eq!(updated.revision, 2);
        assert_eq!(updated.mode, PolicyMode::Enforce);
    }
}
