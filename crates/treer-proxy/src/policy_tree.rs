use crate::policy::{PolicyRequest, PolicySubject};
use sqlx::{PgPool, Row};
use std::collections::HashMap;
use treer_protocol::{
    PolicyTreeContext, ProtocolError, WorkloadFolder, WorkloadPlacement, WorkloadTree,
};

pub(crate) struct TreeSnapshot {
    pub revision: u64,
    nodes: HashMap<String, PolicyTreeContext>,
    agent_machines: HashMap<String, String>,
    service_ids: std::collections::HashSet<String>,
}

impl TreeSnapshot {
    pub fn resolve_path(&self, kind: &str, path: &str) -> Option<String> {
        if kind == "service" {
            return self
                .service_ids
                .iter()
                .find(|id| {
                    self.nodes
                        .get(*id)
                        .is_some_and(|c| c.placed && c.path == path)
                })
                .cloned();
        }
        let mut ids = self
            .nodes
            .values()
            .filter(|c| c.placed && c.kind == kind && c.path == path)
            .map(|c| c.node_id.clone())
            .collect::<Vec<_>>();
        ids.sort();
        ids.dedup();
        (ids.len() == 1).then(|| ids.remove(0))
    }
    pub fn agent_subject(&self, id: &str) -> Option<PolicySubject> {
        self.agent_machines
            .get(id)
            .map(|server| PolicySubject::Agent {
                agent_id: id.into(),
                server_id: server.clone(),
            })
    }
    pub fn is_service(&self, id: &str) -> bool {
        self.service_ids.contains(id)
    }
    pub fn is_resource(&self, kind: &str, id: &str) -> bool {
        self.nodes.get(id).is_some_and(|c| c.kind == kind)
    }
    pub async fn load(pool: &PgPool, workspace: &str) -> Result<Self, ProtocolError> {
        let mut tx = pool.begin().await.map_err(db_error)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        let revision: i64 = sqlx::query_scalar(
            "SELECT tree_revision FROM workspaces WHERE workspace_id=$1 AND deleted_at IS NULL",
        )
        .bind(workspace)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_error)?
        .ok_or_else(|| ProtocolError::new("workspace_not_found", "workspace not found"))?;
        let revision = u64::try_from(revision)
            .map_err(|_| ProtocolError::new("invalid_tree_revision", "invalid Tree revision"))?;
        let folders = sqlx::query("SELECT folder_id,name,parent_id FROM workload_folders WHERE workspace_id=$1 ORDER BY folder_id")
            .bind(workspace).fetch_all(&mut *tx).await.map_err(db_error)?
            .into_iter().map(|r| WorkloadFolder {folder_id:r.get("folder_id"),name:r.get("name"),parent_id:r.get("parent_id")}).collect();
        let placements = sqlx::query(
            "SELECT agent_id,folder_id,position FROM workload_placements WHERE workspace_id=$1",
        )
        .bind(workspace)
        .fetch_all(&mut *tx)
        .await
        .map_err(db_error)?
        .into_iter()
        .map(|r| WorkloadPlacement {
            agent_id: r.get("agent_id"),
            folder_id: r.get("folder_id"),
            position: r.get("position"),
        })
        .collect();
        let tree = WorkloadTree {
            folders,
            placements,
        };
        let mut nodes = HashMap::new();
        let mut agent_machines = HashMap::new();
        let mut service_ids = std::collections::HashSet::new();
        for f in &tree.folders {
            nodes.insert(
                f.folder_id.clone(),
                context(
                    &tree,
                    revision,
                    &f.folder_id,
                    "folder",
                    &f.name,
                    f.parent_id.as_deref(),
                    true,
                )?,
            );
        }
        let rows = sqlx::query("SELECT c.agent_id,c.server_id,COALESCE(a.name,n.name) AS name,a.app_id,a.service_id FROM agent_credentials c LEFT JOIN agent_names n ON n.workspace_id=c.workspace_id AND n.agent_id=c.agent_id LEFT JOIN app_deployments a ON a.workspace_id=c.workspace_id AND (a.runtime_agent_id=c.agent_id OR a.app_id=c.agent_id) WHERE c.workspace_id=$1 AND c.revoked_at IS NULL AND NOT EXISTS(SELECT 1 FROM deleted_agents d WHERE d.workspace_id=c.workspace_id AND d.agent_id=c.agent_id)")
            .bind(workspace).fetch_all(&mut *tx).await.map_err(db_error)?;
        for row in rows {
            let id: String = row.get("agent_id");
            agent_machines.insert(id.clone(), row.get("server_id"));
            let Some(name) = row.get::<Option<String>, _>("name") else {
                continue;
            };
            let app: Option<String> = row.get("app_id");
            let placement = tree.placements.iter().find(|p| p.agent_id == id);
            let ctx = context(
                &tree,
                revision,
                app.as_deref().unwrap_or(&id),
                if app.is_some() { "app" } else { "agent" },
                &name,
                placement.and_then(|p| p.folder_id.as_deref()),
                placement.is_some(),
            )?;
            nodes.insert(id, ctx.clone());
            if let Some(app) = app {
                nodes.insert(app, ctx.clone());
                let service: String = row.get("service_id");
                service_ids.insert(service.clone());
                nodes.insert(service, ctx);
            }
        }
        // Stopped Apps can have no current runtime; their deployment is durable.
        for row in sqlx::query("SELECT app_id,name,service_id,runtime_agent_id FROM app_deployments WHERE workspace_id=$1")
            .bind(workspace).fetch_all(&mut *tx).await.map_err(db_error)? {
            let id: String = row.get("app_id");
            if nodes.contains_key(&id) {continue;}
            let runtime: Option<String> = row.get("runtime_agent_id");
            let placement = tree.placements.iter().find(|p| p.agent_id == runtime.as_deref().unwrap_or(&id));
            let ctx = context(&tree, revision, &id, "app", &row.get::<String,_>("name"), placement.and_then(|p| p.folder_id.as_deref()), placement.is_some())?;
            nodes.insert(id,ctx.clone());
            let service:String=row.get("service_id");
            service_ids.insert(service.clone());
            nodes.insert(service,ctx);
        }
        tx.commit().await.map_err(db_error)?;
        Ok(Self {
            revision,
            nodes,
            agent_machines,
            service_ids,
        })
    }

    pub fn enrich(&self, request: &PolicyRequest) -> PolicyRequest {
        let mut request = request.clone();
        request.tree_revision = Some(self.revision);
        let subject = match &request.subject {
            PolicySubject::Agent { agent_id, .. } => Some(agent_id.as_str()),
            PolicySubject::Service { service_id } => Some(service_id.as_str()),
            _ => None,
        };
        request.subject_tree = subject.and_then(|id| self.nodes.get(id)).cloned();
        let target = if matches!(
            request.resource.kind.as_str(),
            "message" | "network.endpoint"
        ) {
            request
                .resource
                .attributes
                .get("destination_agent_id")
                .map(String::as_str)
        } else if request.resource.kind == "message.mailbox" {
            (request
                .resource
                .attributes
                .get("principal_kind")
                .map(String::as_str)
                == Some("agent"))
            .then_some(request.resource.id.as_str())
        } else if matches!(
            request.resource.kind.as_str(),
            "agent" | "app" | "folder" | "machine.service"
        ) {
            Some(request.resource.id.as_str())
        } else {
            None
        };
        request.resource_tree = target.and_then(|id| self.nodes.get(id)).cloned();
        request
    }
}

fn context(
    tree: &WorkloadTree,
    revision: u64,
    id: &str,
    kind: &str,
    name: &str,
    folder: Option<&str>,
    placed: bool,
) -> Result<PolicyTreeContext, ProtocolError> {
    let folder_path = tree.folder_path(folder)?;
    let mut ancestor_ids = Vec::new();
    let mut next = folder;
    while let Some(id) = next {
        if ancestor_ids.iter().any(|v| v == id) {
            return Err(ProtocolError::new("invalid_workload_tree", "folder cycle"));
        }
        ancestor_ids.push(id.to_string());
        next = tree
            .folders
            .iter()
            .find(|f| f.folder_id == id)
            .and_then(|f| f.parent_id.as_deref());
    }
    ancestor_ids.reverse();
    let mut ancestor_paths = vec!["/".to_string()];
    for id in &ancestor_ids {
        ancestor_paths.push(tree.folder_path(Some(id))?);
    }
    let path = format!("{}/{}", folder_path.trim_end_matches('/'), name);
    Ok(PolicyTreeContext {
        node_id: id.into(),
        kind: kind.into(),
        name: name.into(),
        path,
        folder_id: folder.map(str::to_owned),
        folder_path,
        ancestor_ids,
        ancestor_paths,
        placed,
        tree_revision: revision,
    })
}

fn db_error(_: sqlx::Error) -> ProtocolError {
    ProtocolError::new(
        "policy_tree_unavailable",
        "current policy Tree context unavailable",
    )
}
