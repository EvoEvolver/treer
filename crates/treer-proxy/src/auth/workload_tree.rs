use super::*;
use treer_protocol::{
    CreateWorkloadFolderRequest, SetWorkloadPlacementRequest, UpdateWorkloadFolderRequest,
    WorkloadFolder, WorkloadPlacement, WorkloadTree,
};

async fn load_workload_tree(
    tx: &mut Transaction<'_, Postgres>,
    workspace: &str,
) -> Result<WorkloadTree, AuthFailure> {
    let folders = sqlx::query("SELECT folder_id,name,parent_id FROM workload_folders WHERE workspace_id=$1 ORDER BY lower(name),folder_id")
        .bind(workspace).fetch_all(&mut **tx).await.map_err(AuthFailure::database)?.into_iter().map(folder).collect();
    let placements = sqlx::query("SELECT p.agent_id,p.folder_id,p.position FROM workload_placements p JOIN agent_credentials a ON a.agent_id=p.agent_id AND a.workspace_id=p.workspace_id WHERE p.workspace_id=$1 AND a.revoked_at IS NULL ORDER BY p.folder_id NULLS FIRST,p.position,p.agent_id")
        .bind(workspace).fetch_all(&mut **tx).await.map_err(AuthFailure::database)?.into_iter().map(|r| WorkloadPlacement {agent_id:r.get("agent_id"),folder_id:r.get("folder_id"),position:r.get("position")}).collect();
    Ok(WorkloadTree {
        folders,
        placements,
    })
}

fn invalid(message: &str) -> AuthFailure {
    AuthFailure::bad_request("invalid_workload_tree", message)
}

fn folder(row: sqlx::postgres::PgRow) -> WorkloadFolder {
    WorkloadFolder {
        folder_id: row.get("folder_id"),
        name: row.get("name"),
        parent_id: row.get("parent_id"),
    }
}

async fn read_canonical_workload_paths(
    tx: &mut Transaction<'_, Postgres>,
    workspace: &str,
) -> Result<HashMap<String, (String, String)>, AuthFailure> {
    let tree = load_workload_tree(tx, workspace).await?;
    let rows = sqlx::query("SELECT p.agent_id,COALESCE(n.name,a.name) AS name FROM workload_placements p JOIN agent_credentials c ON c.agent_id=p.agent_id AND c.workspace_id=p.workspace_id LEFT JOIN agent_names n ON n.agent_id=p.agent_id AND n.workspace_id=p.workspace_id LEFT JOIN app_deployments a ON a.workspace_id=p.workspace_id AND COALESCE(a.runtime_agent_id,a.app_id)=p.agent_id WHERE p.workspace_id=$1 AND c.revoked_at IS NULL")
        .bind(workspace).fetch_all(&mut **tx).await.map_err(AuthFailure::database)?;
    let mut paths = HashMap::new();
    for row in rows {
        let id: String = row.get("agent_id");
        if let Some(name) = row.get::<Option<String>, _>("name") {
            let path = tree
                .workload_path(&id, &name)
                .map_err(|_| invalid("invalid canonical workload path"))?;
            paths.insert(id, (name, path));
        }
    }
    Ok(paths)
}

impl AuthStore {
    // Every mutation locks the workspace row before reading the graph. This also
    // serializes simultaneous moves that would otherwise jointly form a cycle.
    pub(super) async fn lock_workload_tree(
        &self,
        workspace: &str,
    ) -> Result<Transaction<'_, Postgres>, AuthFailure> {
        let mut tx = self.pool.begin().await.map_err(AuthFailure::database)?;
        sqlx::query("SELECT workspace_id FROM workspaces WHERE workspace_id=$1 AND deleted_at IS NULL FOR UPDATE")
            .bind(workspace).fetch_optional(&mut *tx).await.map_err(AuthFailure::database)?
            .ok_or_else(|| AuthFailure::not_found("workspace_not_found", "workspace does not exist"))?;
        Ok(tx)
    }

    #[cfg(test)]
    pub async fn create_named_agent_credential(
        &self,
        workspace: &str,
        server: &str,
        agent: &str,
        name: &str,
        folder: Option<&str>,
    ) -> Result<String, AuthFailure> {
        self.create_named_agent_credential_for_caller(workspace, server, agent, name, folder, None)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_named_agent_credential_for_caller(
        &self,
        workspace: &str,
        server: &str,
        agent: &str,
        name: &str,
        folder: Option<&str>,
        caller: Option<&str>,
    ) -> Result<String, AuthFailure> {
        treer_protocol::validate_workload_label(name)
            .map_err(|e| AuthFailure::bad_request("invalid_agent_name", &e.message))?;
        let mut tx = self.lock_workload_tree(workspace).await?;
        let destination = creation_folder(&mut tx, workspace, folder, caller).await?;
        let folder = destination.as_deref();
        validate_parent(&mut tx, workspace, folder, None).await?;
        validate_workload_name(&mut tx, workspace, folder, name, Some(agent)).await?;
        let credential = format!("wlc_{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let record = AgentCredentialRecord {
            workspace_id: workspace.into(),
            server_id: server.into(),
            secret_hash: fast_secret_hash(&credential),
            cached_at: Instant::now(),
        };
        sqlx::query("INSERT INTO agent_credentials(agent_id,workspace_id,server_id,secret_hash,created_at) VALUES($1,$2,$3,$4,$5)").bind(agent).bind(workspace).bind(server).bind(&record.secret_hash).bind(Utc::now().to_rfc3339()).execute(&mut *tx).await.map_err(AuthFailure::database)?;
        sqlx::query(
            "INSERT INTO agent_names(agent_id,workspace_id,name,updated_at) VALUES($1,$2,$3,$4)",
        )
        .bind(agent)
        .bind(workspace)
        .bind(name)
        .bind(Utc::now().to_rfc3339())
        .execute(&mut *tx)
        .await
        .map_err(AuthFailure::database)?;
        sqlx::query("INSERT INTO workload_placements(workspace_id,agent_id,folder_id,position) SELECT $1,$2,$3,COALESCE(MAX(position)+1,0) FROM workload_placements WHERE workspace_id=$1 AND folder_id IS NOT DISTINCT FROM $3").bind(workspace).bind(agent).bind(folder).execute(&mut *tx).await.map_err(AuthFailure::database)?;
        audit::insert_workspace_mutation(
            &mut tx,
            workspace,
            "tree.placement.create",
            "agent",
            agent,
        )
        .await
        .map_err(AuthFailure::database)?;
        tx.commit().await.map_err(AuthFailure::database)?;
        self.agent_credentials
            .write()
            .await
            .insert(agent.into(), record);
        Ok(credential)
    }

    pub async fn workload_tree(&self, workspace: &str) -> Result<WorkloadTree, AuthFailure> {
        let mut tx = self.lock_workload_tree(workspace).await?;
        let tree = load_workload_tree(&mut tx, workspace).await?;
        tx.commit().await.map_err(AuthFailure::database)?;
        Ok(tree)
    }

    async fn tree_read_snapshot(
        &self,
        workspace: &str,
    ) -> Result<Transaction<'_, Postgres>, AuthFailure> {
        let mut tx = self.pool.begin().await.map_err(AuthFailure::database)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(AuthFailure::database)?;
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM workspaces WHERE workspace_id=$1 AND deleted_at IS NULL)",
        )
        .bind(workspace)
        .fetch_one(&mut *tx)
        .await
        .map_err(AuthFailure::database)?;
        if !exists {
            return Err(AuthFailure::not_found(
                "workspace_not_found",
                "workspace does not exist",
            ));
        }
        Ok(tx)
    }

    /// Read names and placement from one read-only MVCC snapshot, without
    /// serializing App directory requests with Tree writes on any Proxy.
    /// Unplaced and revoked identities are deliberately absent.
    pub async fn canonical_workload_paths(
        &self,
        workspace: &str,
    ) -> Result<HashMap<String, (String, String)>, AuthFailure> {
        let mut tx = self.tree_read_snapshot(workspace).await?;
        let paths = read_canonical_workload_paths(&mut tx, workspace).await?;
        tx.commit().await.map_err(AuthFailure::database)?;
        Ok(paths)
    }

    pub async fn create_workload_folder(
        &self,
        workspace: &str,
        request: CreateWorkloadFolderRequest,
    ) -> Result<WorkloadFolder, AuthFailure> {
        let name = validate_folder_name(&request.name)?;
        let mut tx = self.lock_workload_tree(workspace).await?;
        validate_parent(&mut tx, workspace, request.parent_id.as_deref(), None).await?;
        validate_sibling_name(
            &mut tx,
            workspace,
            request.parent_id.as_deref(),
            &name,
            None,
        )
        .await?;
        let result = WorkloadFolder {
            folder_id: format!("folder_{}", Uuid::new_v4().simple()),
            name,
            parent_id: request.parent_id,
        };
        sqlx::query("INSERT INTO workload_folders(workspace_id,folder_id,name,parent_id) VALUES($1,$2,$3,$4)").bind(workspace).bind(&result.folder_id).bind(&result.name).bind(&result.parent_id).execute(&mut *tx).await.map_err(AuthFailure::database)?;
        audit::insert_workspace_mutation(
            &mut tx,
            workspace,
            "tree.folder.create",
            "folder",
            &result.folder_id,
        )
        .await
        .map_err(AuthFailure::database)?;
        tx.commit().await.map_err(AuthFailure::database)?;
        Ok(result)
    }

    pub async fn update_workload_folder(
        &self,
        workspace: &str,
        id: &str,
        request: UpdateWorkloadFolderRequest,
    ) -> Result<WorkloadFolder, AuthFailure> {
        let mut tx = self.lock_workload_tree(workspace).await?;
        let mut result = load_folder(&mut tx, workspace, id).await?;
        let original = result.clone();
        if let Some(name) = request.name {
            result.name = validate_folder_name(&name)?;
        }
        if let Some(parent) = request.parent_id {
            validate_parent(&mut tx, workspace, parent.as_deref(), Some(id)).await?;
            result.parent_id = parent;
        }
        validate_sibling_name(
            &mut tx,
            workspace,
            result.parent_id.as_deref(),
            &result.name,
            Some(id),
        )
        .await?;
        if result.name == original.name && result.parent_id == original.parent_id {
            return Ok(result);
        }
        sqlx::query("UPDATE workload_folders SET name=$3,parent_id=$4 WHERE workspace_id=$1 AND folder_id=$2").bind(workspace).bind(id).bind(&result.name).bind(&result.parent_id).execute(&mut *tx).await.map_err(AuthFailure::database)?;
        audit::insert_workspace_mutation(&mut tx, workspace, "tree.folder.update", "folder", id)
            .await
            .map_err(AuthFailure::database)?;
        tx.commit().await.map_err(AuthFailure::database)?;
        Ok(result)
    }

    pub async fn delete_workload_folder(
        &self,
        workspace: &str,
        id: &str,
    ) -> Result<(), AuthFailure> {
        self.remove_workload_folder(workspace, id, false).await
    }

    pub async fn remove_workload_folder(
        &self,
        workspace: &str,
        id: &str,
        require_empty: bool,
    ) -> Result<(), AuthFailure> {
        let mut tx = self.lock_workload_tree(workspace).await?;
        let result = load_folder(&mut tx, workspace, id).await?;
        if require_empty {
            let occupied: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM workload_folders WHERE workspace_id=$1 AND parent_id=$2) OR EXISTS(SELECT 1 FROM workload_placements p JOIN agent_credentials a ON a.workspace_id=p.workspace_id AND a.agent_id=p.agent_id WHERE p.workspace_id=$1 AND p.folder_id=$2 AND a.revoked_at IS NULL)")
                .bind(workspace).bind(id).fetch_one(&mut *tx).await.map_err(AuthFailure::database)?;
            if occupied {
                return Err(AuthFailure::bad_request(
                    "folder_not_empty",
                    "folder is not empty",
                ));
            }
        }
        sqlx::query(
            "UPDATE workload_folders SET parent_id=$3 WHERE workspace_id=$1 AND parent_id=$2",
        )
        .bind(workspace)
        .bind(id)
        .bind(&result.parent_id)
        .execute(&mut *tx)
        .await
        .map_err(AuthFailure::database)?;
        let promoted = sqlx::query("SELECT p.agent_id,COALESCE(n.name,a.name) AS name FROM workload_placements p LEFT JOIN agent_names n ON p.workspace_id=n.workspace_id AND p.agent_id=n.agent_id LEFT JOIN app_deployments a ON a.workspace_id=p.workspace_id AND (a.runtime_agent_id=p.agent_id OR a.app_id=p.agent_id) WHERE p.workspace_id=$1 AND p.folder_id=$2 AND COALESCE(n.name,a.name) IS NOT NULL").bind(workspace).bind(id).fetch_all(&mut *tx).await.map_err(AuthFailure::database)?;
        for row in promoted {
            validate_workload_name(
                &mut tx,
                workspace,
                result.parent_id.as_deref(),
                row.get::<String, _>("name").as_str(),
                Some(row.get::<String, _>("agent_id").as_str()),
            )
            .await?;
        }
        // Append promoted placements after existing siblings, preserving both orders.
        sqlx::query("UPDATE workload_placements SET position=position+(SELECT COALESCE(MAX(position)+1,0) FROM workload_placements WHERE workspace_id=$1 AND folder_id IS NOT DISTINCT FROM $3),folder_id=$3 WHERE workspace_id=$1 AND folder_id=$2").bind(workspace).bind(id).bind(&result.parent_id).execute(&mut *tx).await.map_err(AuthFailure::database)?;
        sqlx::query("DELETE FROM workload_folders WHERE workspace_id=$1 AND folder_id=$2")
            .bind(workspace)
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(AuthFailure::database)?;
        normalize(&mut tx, workspace).await?;
        audit::insert_workspace_mutation(&mut tx, workspace, "tree.folder.delete", "folder", id)
            .await
            .map_err(AuthFailure::database)?;
        tx.commit().await.map_err(AuthFailure::database)?;
        Ok(())
    }

    pub async fn set_workload_placement(
        &self,
        workspace: &str,
        agent: &str,
        request: SetWorkloadPlacementRequest,
    ) -> Result<WorkloadPlacement, AuthFailure> {
        if request.position < 0 {
            return Err(invalid("position must be nonnegative"));
        }
        let mut tx = self.lock_workload_tree(workspace).await?;
        sqlx::query("SELECT agent_id FROM agent_credentials WHERE workspace_id=$1 AND agent_id=$2 AND revoked_at IS NULL FOR SHARE").bind(workspace).bind(agent).fetch_optional(&mut *tx).await.map_err(AuthFailure::database)?.ok_or_else(|| invalid("agent does not exist in this workspace"))?;
        validate_parent(&mut tx, workspace, request.folder_id.as_deref(), None).await?;
        let name: Option<String> = sqlx::query_scalar(
            "SELECT COALESCE((SELECT name FROM agent_names WHERE workspace_id=$1 AND agent_id=$2),(SELECT name FROM app_deployments WHERE workspace_id=$1 AND (runtime_agent_id=$2 OR app_id=$2)))",
        )
        .bind(workspace)
        .bind(agent)
        .fetch_one(&mut *tx)
        .await
        .map_err(AuthFailure::database)?;
        if let Some(name) = name {
            validate_workload_name(
                &mut tx,
                workspace,
                request.folder_id.as_deref(),
                &name,
                Some(agent),
            )
            .await?;
        }

        let original: Option<(Option<String>, i64)> = sqlx::query_as("SELECT folder_id,position FROM workload_placements WHERE workspace_id=$1 AND agent_id=$2")
            .bind(workspace).bind(agent).fetch_optional(&mut *tx).await.map_err(AuthFailure::database)?;
        sqlx::query("DELETE FROM workload_placements WHERE workspace_id=$1 AND agent_id=$2")
            .bind(workspace)
            .bind(agent)
            .execute(&mut *tx)
            .await
            .map_err(AuthFailure::database)?;
        normalize(&mut tx, workspace).await?;
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workload_placements WHERE workspace_id=$1 AND folder_id IS NOT DISTINCT FROM $2").bind(workspace).bind(&request.folder_id).fetch_one(&mut *tx).await.map_err(AuthFailure::database)?;
        let position = request.position.min(count);
        sqlx::query("UPDATE workload_placements SET position=position+1 WHERE workspace_id=$1 AND folder_id IS NOT DISTINCT FROM $2 AND position >= $3").bind(workspace).bind(&request.folder_id).bind(position).execute(&mut *tx).await.map_err(AuthFailure::database)?;
        sqlx::query("INSERT INTO workload_placements(workspace_id,agent_id,folder_id,position) VALUES($1,$2,$3,$4)").bind(workspace).bind(agent).bind(&request.folder_id).bind(position).execute(&mut *tx).await.map_err(AuthFailure::database)?;
        if original.as_ref() != Some(&(request.folder_id.clone(), position)) {
            audit::insert_workspace_mutation(
                &mut tx,
                workspace,
                "tree.placement.update",
                "agent",
                agent,
            )
            .await
            .map_err(AuthFailure::database)?;
        }
        tx.commit().await.map_err(AuthFailure::database)?;
        Ok(WorkloadPlacement {
            agent_id: agent.into(),
            folder_id: request.folder_id,
            position,
        })
    }
}

/// Called under the workspace mutation lock: inheritance and placement are one
/// transaction even when callers or folders move on another Proxy replica.
pub(super) async fn creation_folder(
    tx: &mut Transaction<'_, Postgres>,
    workspace: &str,
    requested: Option<&str>,
    caller: Option<&str>,
) -> Result<Option<String>, AuthFailure> {
    let destination = match requested {
        Some(treer_protocol::WORKLOAD_CREATE_ROOT) => None,
        Some(folder) => Some(folder.to_owned()),
        None => match caller {
            None => None,
            Some(caller) => sqlx::query_scalar::<_, Option<String>>("SELECT p.folder_id FROM workload_placements p JOIN agent_credentials a ON a.workspace_id=p.workspace_id AND a.agent_id=p.agent_id WHERE p.workspace_id=$1 AND p.agent_id=$2 AND a.revoked_at IS NULL AND NOT EXISTS(SELECT 1 FROM deleted_agents d WHERE d.workspace_id=p.workspace_id AND d.agent_id=p.agent_id)")
                .bind(workspace).bind(caller).fetch_optional(&mut **tx).await.map_err(AuthFailure::database)?
                .ok_or_else(|| AuthFailure::bad_request("caller_unplaced", "caller has no active placement; request an explicitly authorized folder or ask the owner to place it"))?,
        },
    };
    validate_parent(tx, workspace, destination.as_deref(), None).await?;
    Ok(destination)
}

fn validate_folder_name(name: &str) -> Result<String, AuthFailure> {
    let name = validate_resource_name(name, "folder")?;
    if name.contains('/') || matches!(name.as_str(), "." | "..") {
        return Err(invalid("folder names cannot contain / or equal . or .."));
    }
    Ok(name)
}

pub(super) async fn validate_workload_name(
    tx: &mut Transaction<'_, Postgres>,
    workspace: &str,
    folder: Option<&str>,
    name: &str,
    excluding: Option<&str>,
) -> Result<(), AuthFailure> {
    let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM agent_names n LEFT JOIN workload_placements p ON p.workspace_id=n.workspace_id AND p.agent_id=n.agent_id LEFT JOIN agent_credentials c ON c.workspace_id=n.workspace_id AND c.agent_id=n.agent_id WHERE n.workspace_id=$1 AND n.name=$3 AND p.folder_id IS NOT DISTINCT FROM $2 AND ($4::text IS NULL OR n.agent_id<>$4) AND c.revoked_at IS NULL AND NOT EXISTS(SELECT 1 FROM deleted_agents d WHERE d.workspace_id=n.workspace_id AND d.agent_id=n.agent_id)) OR EXISTS(SELECT 1 FROM app_deployments a LEFT JOIN workload_placements p ON p.workspace_id=a.workspace_id AND p.agent_id=COALESCE(a.runtime_agent_id,a.app_id) WHERE a.workspace_id=$1 AND a.name=$3 AND p.folder_id IS NOT DISTINCT FROM $2 AND ($4::text IS NULL OR COALESCE(a.runtime_agent_id,a.app_id)<>$4))")
        .bind(workspace).bind(folder).bind(name).bind(excluding).fetch_one(&mut **tx).await.map_err(AuthFailure::database)?;
    if exists {
        return Err(AuthFailure::conflict(
            "agent_name_conflict",
            "an Agent or App in the destination folder already has this name",
        ));
    }
    Ok(())
}

async fn validate_sibling_name(
    tx: &mut Transaction<'_, Postgres>,
    workspace: &str,
    parent: Option<&str>,
    name: &str,
    excluding: Option<&str>,
) -> Result<(), AuthFailure> {
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM workload_folders WHERE workspace_id=$1 AND parent_id IS NOT DISTINCT FROM $2 AND name=$3 AND ($4::text IS NULL OR folder_id<>$4))")
        .bind(workspace).bind(parent).bind(name).bind(excluding).fetch_one(&mut **tx).await.map_err(AuthFailure::database)?;
    if exists {
        return Err(AuthFailure::bad_request(
            "folder_exists",
            "a folder with this name already exists in the destination",
        ));
    }
    Ok(())
}

async fn load_folder(
    tx: &mut Transaction<'_, Postgres>,
    workspace: &str,
    id: &str,
) -> Result<WorkloadFolder, AuthFailure> {
    sqlx::query("SELECT folder_id,name,parent_id FROM workload_folders WHERE workspace_id=$1 AND folder_id=$2").bind(workspace).bind(id).fetch_optional(&mut **tx).await.map_err(AuthFailure::database)?.map(folder).ok_or_else(|| AuthFailure::not_found("folder_not_found","folder does not exist in this workspace"))
}

async fn validate_parent(
    tx: &mut Transaction<'_, Postgres>,
    workspace: &str,
    parent: Option<&str>,
    moving: Option<&str>,
) -> Result<(), AuthFailure> {
    let mut next = parent.map(str::to_owned);
    let mut seen = std::collections::HashSet::new();
    while let Some(id) = next {
        if moving == Some(id.as_str()) || !seen.insert(id.clone()) {
            return Err(invalid("folder move would create a cycle"));
        }
        next = load_folder(tx, workspace, &id).await?.parent_id;
    }
    Ok(())
}

async fn normalize(tx: &mut Transaction<'_, Postgres>, workspace: &str) -> Result<(), AuthFailure> {
    sqlx::query("WITH ranked AS (SELECT agent_id,ROW_NUMBER() OVER(PARTITION BY folder_id ORDER BY position,agent_id)-1 AS pos FROM workload_placements WHERE workspace_id=$1) UPDATE workload_placements p SET position=r.pos FROM ranked r WHERE p.workspace_id=$1 AND p.agent_id=r.agent_id").bind(workspace).execute(&mut **tx).await.map_err(AuthFailure::database)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn canonical_path_snapshot_does_not_block_writes_or_mix_revisions() {
        let store = setup().await;
        let original = store
            .create_workload_folder(
                "one",
                CreateWorkloadFolderRequest {
                    name: "original".into(),
                    parent_id: None,
                },
            )
            .await
            .unwrap();
        store
            .create_named_agent_credential(
                "one",
                "srv",
                "snapshot-agent",
                "before",
                Some(&original.folder_id),
            )
            .await
            .unwrap();
        let mut snapshot = store.tree_read_snapshot("one").await.unwrap();
        // This selects folders/placement before the concurrent committed changes.
        let before = read_canonical_workload_paths(&mut snapshot, "one")
            .await
            .unwrap();
        assert_eq!(before["snapshot-agent"].1, "/original/before");
        tokio::time::timeout(StdDuration::from_secs(5), async {
            store
                .create_named_agent_credential("one", "srv", "new-agent", "new", None)
                .await
                .unwrap();
            store
                .set_agent_name("one", "snapshot-agent", "after")
                .await
                .unwrap();
            store
                .update_workload_folder(
                    "one",
                    &original.folder_id,
                    UpdateWorkloadFolderRequest {
                        name: Some("renamed".into()),
                        parent_id: None,
                    },
                )
                .await
                .unwrap();
        })
        .await
        .expect("open directory snapshot must not block creation/rename writes");
        let retained = read_canonical_workload_paths(&mut snapshot, "one")
            .await
            .unwrap();
        assert_eq!(
            retained, before,
            "folders, placement and names remain one consistent revision"
        );
        snapshot.commit().await.unwrap();
        let current = store.canonical_workload_paths("one").await.unwrap();
        assert_eq!(current["snapshot-agent"].1, "/renamed/after");
        assert_eq!(current["new-agent"].1, "/new");
    }

    fn app_request(name: &str, folder: Option<&str>) -> treer_protocol::CreateAppDeploymentRequest {
        treer_protocol::CreateAppDeploymentRequest {
            folder_id: folder.map(str::to_owned),
            server_id: None,
            name: name.into(),
            command: "/bin/true".into(),
            args: vec![],
            cwd: ".".into(),
            port: 8888,
            hostname: String::new(),
            public: false,
        }
    }

    async fn stored_folder(store: &AuthStore, id: &str) -> Option<String> {
        sqlx::query_scalar(
            "SELECT folder_id FROM workload_placements WHERE workspace_id='one' AND agent_id=$1",
        )
        .bind(id)
        .fetch_one(&store.pool())
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn trusted_creation_inherits_agent_and_app_folder_in_the_creation_transaction() {
        let store = setup().await;
        let folder = store
            .create_workload_folder(
                "one",
                CreateWorkloadFolderRequest {
                    name: "team".into(),
                    parent_id: None,
                },
            )
            .await
            .unwrap();
        store
            .create_named_agent_credential(
                "one",
                "machine",
                "caller",
                "caller",
                Some(&folder.folder_id),
            )
            .await
            .unwrap();
        store
            .create_named_agent_credential_for_caller(
                "one",
                "machine",
                "child",
                "child",
                None,
                Some("caller"),
            )
            .await
            .unwrap();
        assert_eq!(
            stored_folder(&store, "child").await,
            Some(folder.folder_id.clone())
        );
        let app = store
            .create_app_deployment_for_caller(
                "one",
                "caller",
                "machine".into(),
                app_request("mail", None),
                Some("caller"),
            )
            .await
            .unwrap();
        assert_eq!(
            stored_folder(&store, &app.app_id).await,
            Some(folder.folder_id.clone())
        );
        // Explicit root is distinct from omission; App callers inherit as well.
        store
            .create_named_agent_credential("one", "machine", "root", "root", None)
            .await
            .unwrap();
        let root_app = store
            .create_app_deployment_for_caller(
                "one",
                "root",
                "machine".into(),
                app_request("root-app", None),
                Some("root"),
            )
            .await
            .unwrap();
        assert_eq!(stored_folder(&store, &root_app.app_id).await, None);
        let explicit = store
            .create_app_deployment_for_caller(
                "one",
                "caller",
                "machine".into(),
                app_request("explicit-root", Some("/")),
                Some("caller"),
            )
            .await
            .unwrap();
        assert_eq!(stored_folder(&store, &explicit.app_id).await, None);
        store
            .rotate_agent_credential("one", "machine", &app.app_id)
            .await
            .unwrap();
        let child_app = store
            .create_app_deployment_for_caller(
                "one",
                "app-caller",
                "machine".into(),
                app_request("child-app", None),
                Some(&app.app_id),
            )
            .await
            .unwrap();
        assert_eq!(
            stored_folder(&store, &child_app.app_id).await,
            Some(folder.folder_id)
        );
    }

    #[tokio::test]
    async fn missing_or_cross_workspace_caller_cannot_silently_create_at_root() {
        let store = setup().await;
        store
            .create_agent_credential("one", "machine", "unplaced")
            .await
            .unwrap();
        store
            .set_workload_placement(
                "two",
                "foreign",
                SetWorkloadPlacementRequest {
                    folder_id: None,
                    position: 0,
                },
            )
            .await
            .unwrap();
        for caller in ["unplaced", "foreign"] {
            let error = store
                .create_app_deployment_for_caller(
                    "one",
                    "caller",
                    "machine".into(),
                    app_request("unexpected", None),
                    Some(caller),
                )
                .await
                .unwrap_err();
            assert_eq!(error.code(), "caller_unplaced");
            let error = store
                .create_named_agent_credential_for_caller(
                    "one",
                    "machine",
                    "unexpected",
                    "unexpected",
                    None,
                    Some(caller),
                )
                .await
                .unwrap_err();
            assert_eq!(error.code(), "caller_unplaced");
        }
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM workload_placements WHERE workspace_id='one'")
                .fetch_one(&store.pool())
                .await
                .unwrap();
        assert_eq!(count, 0);
        let human = store
            .create_app_deployment("one", "owner", "machine".into(), app_request("human", None))
            .await
            .unwrap();
        assert_eq!(stored_folder(&store, &human.app_id).await, None);
        let absent: bool = sqlx::query_scalar(
            "SELECT NOT EXISTS(SELECT 1 FROM workload_placements WHERE agent_id='unplaced')",
        )
        .fetch_one(&store.pool())
        .await
        .unwrap();
        assert!(absent, "existing unplaced identities must remain untouched");
        store
            .delete_app_deployment("one", &human.app_id)
            .await
            .unwrap();
        let removed: bool = sqlx::query_scalar("SELECT NOT EXISTS(SELECT 1 FROM workload_placements WHERE agent_id=$1) AND NOT EXISTS(SELECT 1 FROM agent_credentials WHERE agent_id=$1 AND revoked_at IS NULL)").bind(&human.app_id).fetch_one(&store.pool()).await.unwrap();
        assert!(
            removed,
            "deleting an unstarted App removes its placement and revokes its reserved identity"
        );
    }

    #[tokio::test]
    async fn separate_proxy_stores_share_inherited_placement_and_name_reservations() {
        let store = setup().await;
        let mut other = store.clone();
        other.virtual_hosts_update = Arc::default();
        let folder = store
            .create_workload_folder(
                "one",
                CreateWorkloadFolderRequest {
                    name: "team".into(),
                    parent_id: None,
                },
            )
            .await
            .unwrap();
        store
            .create_named_agent_credential(
                "one",
                "machine",
                "caller",
                "caller",
                Some(&folder.folder_id),
            )
            .await
            .unwrap();
        let (agent, app) = tokio::join!(
            store.create_named_agent_credential_for_caller(
                "one",
                "machine",
                "child",
                "shared",
                None,
                Some("caller")
            ),
            other.create_app_deployment_for_caller(
                "one",
                "caller",
                "machine".into(),
                app_request("shared", None),
                Some("caller")
            )
        );
        assert_ne!(
            agent.is_ok(),
            app.is_ok(),
            "exactly one same-folder name may commit across replicas"
        );
        let id = if let Ok(app) = app {
            app.app_id
        } else {
            "child".into()
        };
        assert_eq!(stored_folder(&store, &id).await, Some(folder.folder_id));
    }

    async fn setup() -> AuthStore {
        let store = AuthStore::for_test("password").await;
        store.seed_test_workspace("one").await;
        store.seed_test_workspace("two").await;
        for (id, ws) in [("a", "one"), ("b", "one"), ("c", "one"), ("foreign", "two")] {
            sqlx::query("INSERT INTO agent_credentials(agent_id,workspace_id,server_id,secret_hash,created_at) VALUES($1,$2,'machine','hash','now')").bind(id).bind(ws).execute(&store.pool).await.unwrap();
        }
        store
    }

    #[tokio::test]
    async fn workload_names_are_unique_per_folder_for_creation_rename_and_moves() {
        let store = setup().await;
        let a = create(&store, "one", "team-a", None).await;
        let b = create(&store, "one", "team-b", None).await;
        store
            .create_named_agent_credential("one", "machine", "ag_a", "reviewer", Some(&a.folder_id))
            .await
            .unwrap();
        store
            .create_named_agent_credential("one", "machine", "ag_b", "reviewer", Some(&b.folder_id))
            .await
            .unwrap();
        let conflict = store
            .create_named_agent_credential(
                "one",
                "machine",
                "ag_duplicate",
                "reviewer",
                Some(&a.folder_id),
            )
            .await
            .unwrap_err();
        assert_eq!(conflict.code(), "agent_name_conflict");
        assert!(!sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM agent_credentials WHERE agent_id='ag_duplicate')"
        )
        .fetch_one(&store.pool)
        .await
        .unwrap());
        store
            .create_named_agent_credential("one", "machine", "ag_c", "helper", Some(&a.folder_id))
            .await
            .unwrap();
        assert_eq!(
            store
                .set_agent_name("one", "ag_c", "reviewer")
                .await
                .unwrap_err()
                .code(),
            "agent_name_conflict"
        );
        store
            .set_agent_name("one", "ag_c", "renamed")
            .await
            .unwrap();
        assert_eq!(
            store
                .set_workload_placement(
                    "one",
                    "ag_b",
                    SetWorkloadPlacementRequest {
                        folder_id: Some(a.folder_id.clone()),
                        position: 0
                    }
                )
                .await
                .unwrap_err()
                .code(),
            "agent_name_conflict"
        );
        assert_eq!(
            store
                .workload_tree("one")
                .await
                .unwrap()
                .containing_folder("ag_b"),
            Some(b.folder_id.as_str())
        );
        store
            .create_named_agent_credential("one", "machine", "ag_root", "reviewer", None)
            .await
            .unwrap();
        assert_eq!(
            store
                .set_workload_placement(
                    "one",
                    "ag_b",
                    SetWorkloadPlacementRequest {
                        folder_id: None,
                        position: 0
                    }
                )
                .await
                .unwrap_err()
                .code(),
            "agent_name_conflict"
        );
        assert_eq!(
            store
                .delete_workload_folder("one", &b.folder_id)
                .await
                .unwrap_err()
                .code(),
            "agent_name_conflict"
        );
        // Atomic reservations serialize concurrent callers with the same folder/name.
        let (one, two) = tokio::join!(
            store.create_named_agent_credential("one", "machine", "ag_race1", "race", None),
            store.create_named_agent_credential("one", "machine", "ag_race2", "race", None)
        );
        assert_eq!(usize::from(one.is_ok()) + usize::from(two.is_ok()), 1);
    }

    #[tokio::test]
    async fn apps_and_agents_share_folder_names_but_owned_services_keep_global_names() {
        let store = setup().await;
        let folder = create(&store, "one", "team", None).await;
        let request = || treer_protocol::CreateAppDeploymentRequest {
            folder_id: None,
            server_id: Some("machine".into()),
            name: "docs".into(),
            command: "serve".into(),
            args: vec![],
            cwd: ".".into(),
            port: 8080,
            hostname: String::new(),
            public: false,
        };
        let app = store
            .create_app_deployment("one", "owner", "machine".into(), request())
            .await
            .unwrap();
        assert_eq!(
            store
                .create_named_agent_credential("one", "machine", "ag_conflict", "docs", None)
                .await
                .unwrap_err()
                .code(),
            "agent_name_conflict"
        );
        store
            .rotate_agent_credential("one", "machine", &app.app_id)
            .await
            .unwrap();
        store
            .set_workload_placement(
                "one",
                &app.app_id,
                SetWorkloadPlacementRequest {
                    folder_id: Some(folder.folder_id.clone()),
                    position: 0,
                },
            )
            .await
            .unwrap();
        let second = store
            .create_app_deployment("one", "owner", "machine".into(), request())
            .await
            .unwrap();
        assert_ne!(app.app_id, second.app_id);
        let names: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM machine_services WHERE workspace_id='one' ORDER BY name",
        )
        .fetch_all(&store.pool)
        .await
        .unwrap();
        assert_eq!(names.len(), 2);
        assert_ne!(names[0], names[1]);
        store
            .rotate_agent_credential("one", "machine", &second.app_id)
            .await
            .unwrap();
        assert_eq!(
            store
                .set_workload_placement(
                    "one",
                    &second.app_id,
                    SetWorkloadPlacementRequest {
                        folder_id: Some(folder.folder_id),
                        position: 0
                    }
                )
                .await
                .unwrap_err()
                .code(),
            "agent_name_conflict"
        );
    }

    async fn create(
        store: &AuthStore,
        ws: &str,
        name: &str,
        parent: Option<&str>,
    ) -> WorkloadFolder {
        store
            .create_workload_folder(
                ws,
                CreateWorkloadFolderRequest {
                    name: name.into(),
                    parent_id: parent.map(str::to_owned),
                },
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn workload_tree_unique_siblings_and_empty_removal() {
        let store = setup().await;
        let request = || CreateWorkloadFolderRequest {
            name: "same".into(),
            parent_id: None,
        };
        let (left, right) = tokio::join!(
            store.create_workload_folder("one", request()),
            store.create_workload_folder("one", request())
        );
        assert_ne!(left.is_ok(), right.is_ok());
        let parent = left.or(right).unwrap();
        let child = create(&store, "one", "nested", Some(&parent.folder_id)).await;
        let error = store
            .remove_workload_folder("one", &parent.folder_id, true)
            .await
            .unwrap_err();
        assert_eq!(error.code(), "folder_not_empty");
        assert_eq!(store.workload_tree("one").await.unwrap().folders.len(), 2);
        store
            .remove_workload_folder("one", &child.folder_id, true)
            .await
            .unwrap();
        store
            .remove_workload_folder("one", &parent.folder_id, true)
            .await
            .unwrap();
        assert!(store.workload_tree("one").await.unwrap().folders.is_empty());
    }

    #[tokio::test]
    async fn workload_tree_isolation_cycles_and_concurrent_moves() {
        let store = setup().await;
        let a = create(&store, "one", "A", None).await;
        let b = create(&store, "one", "B", None).await;
        assert!(store
            .create_workload_folder(
                "two",
                CreateWorkloadFolderRequest {
                    name: "bad".into(),
                    parent_id: Some(a.folder_id.clone())
                }
            )
            .await
            .is_err());
        assert!(store
            .set_workload_placement(
                "one",
                "foreign",
                SetWorkloadPlacementRequest {
                    folder_id: None,
                    position: 0
                }
            )
            .await
            .is_err());
        assert!(store
            .set_workload_placement(
                "one",
                "invented",
                SetWorkloadPlacementRequest {
                    folder_id: None,
                    position: 0
                }
            )
            .await
            .is_err());
        assert!(store
            .update_workload_folder(
                "two",
                &a.folder_id,
                UpdateWorkloadFolderRequest {
                    name: Some("stolen".into()),
                    parent_id: None
                }
            )
            .await
            .is_err());
        let (left, right) = tokio::join!(
            store.update_workload_folder(
                "one",
                &a.folder_id,
                UpdateWorkloadFolderRequest {
                    name: None,
                    parent_id: Some(Some(b.folder_id.clone()))
                }
            ),
            store.update_workload_folder(
                "one",
                &b.folder_id,
                UpdateWorkloadFolderRequest {
                    name: None,
                    parent_id: Some(Some(a.folder_id.clone()))
                }
            )
        );
        assert_ne!(
            left.is_ok(),
            right.is_ok(),
            "one simultaneous opposing move must fail"
        );
        assert!(store
            .update_workload_folder(
                "one",
                &a.folder_id,
                UpdateWorkloadFolderRequest {
                    name: None,
                    parent_id: Some(Some(a.folder_id.clone()))
                }
            )
            .await
            .is_err());
        assert!(store.workload_tree("two").await.unwrap().folders.is_empty());
    }

    #[tokio::test]
    async fn workload_tree_promotes_children_reorders_and_persists() {
        let store = setup().await;
        let parent = create(&store, "one", "parent", None).await;
        let doomed = create(&store, "one", "doomed", Some(&parent.folder_id)).await;
        let child = create(&store, "one", "child", Some(&doomed.folder_id)).await;
        for (id, target) in [
            ("a", &parent.folder_id),
            ("b", &doomed.folder_id),
            ("c", &doomed.folder_id),
        ] {
            store
                .set_workload_placement(
                    "one",
                    id,
                    SetWorkloadPlacementRequest {
                        folder_id: Some(target.clone()),
                        position: 100,
                    },
                )
                .await
                .unwrap();
        }
        store
            .delete_workload_folder("one", &doomed.folder_id)
            .await
            .unwrap();
        let tree = store.workload_tree("one").await.unwrap();
        assert_eq!(
            tree.folders
                .iter()
                .find(|f| f.folder_id == child.folder_id)
                .unwrap()
                .parent_id,
            Some(parent.folder_id.clone())
        );
        assert_eq!(
            tree.placements
                .iter()
                .map(|p| (p.agent_id.as_str(), p.position))
                .collect::<Vec<_>>(),
            vec![("a", 0), ("b", 1), ("c", 2)]
        );
        store
            .set_workload_placement(
                "one",
                "c",
                SetWorkloadPlacementRequest {
                    folder_id: Some(parent.folder_id.clone()),
                    position: 0,
                },
            )
            .await
            .unwrap();
        let mut fresh = store.clone();
        fresh.pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(store.pool.connect_options().as_ref().clone())
            .await
            .unwrap();
        // The test pool's schema is session-local: use a separately acquired connection
        // with the same search path to check that organization lives in PostgreSQL.
        let schema: String = sqlx::query_scalar("SELECT current_schema()")
            .fetch_one(&store.pool)
            .await
            .unwrap();
        sqlx::query(&format!("SET search_path TO {schema}"))
            .execute(&fresh.pool)
            .await
            .unwrap();
        let tree = fresh.workload_tree("one").await.unwrap();
        assert_eq!(
            tree.placements
                .iter()
                .map(|p| p.agent_id.as_str())
                .collect::<Vec<_>>(),
            vec!["c", "a", "b"]
        );
        store
            .delete_workload_folder("one", &parent.folder_id)
            .await
            .unwrap();
        let tree = store.workload_tree("one").await.unwrap();
        assert!(tree.placements.iter().all(|p| p.folder_id.is_none()));
        assert!(tree.folders.iter().all(|f| f.parent_id.is_none()));
    }

    #[test]
    fn workload_tree_parent_patch_distinguishes_omitted_and_null() {
        let absent: UpdateWorkloadFolderRequest =
            serde_json::from_value(json!({"name":"renamed"})).unwrap();
        let root: UpdateWorkloadFolderRequest =
            serde_json::from_value(json!({"parent_id":null})).unwrap();
        assert_eq!(absent.parent_id, None);
        assert_eq!(root.parent_id, Some(None));
    }

    #[tokio::test]
    async fn workload_lifecycle_distinct_guards_leave_query_capacity() {
        let store = setup().await;
        let first = async {
            let guard = store.workload_lifecycle_guard("one", "a").await.unwrap();
            tokio::task::yield_now().await;
            let _: i64 = sqlx::query_scalar("SELECT 1::bigint")
                .fetch_one(&store.pool)
                .await
                .unwrap();
            guard.commit().await.unwrap();
        };
        let second = async {
            let guard = store.workload_lifecycle_guard("one", "b").await.unwrap();
            let _: i64 = sqlx::query_scalar("SELECT 1::bigint")
                .fetch_one(&store.pool)
                .await
                .unwrap();
            guard.commit().await.unwrap();
        };
        tokio::time::timeout(StdDuration::from_secs(5), async {
            tokio::join!(first, second);
        })
        .await
        .expect("distinct lifecycle mutations must leave capacity for their queries");
    }
}
