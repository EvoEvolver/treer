use super::*;
use treer_protocol::{
    CreateWorkloadFolderRequest, SetWorkloadPlacementRequest, UpdateWorkloadFolderRequest,
    WorkloadFolder, WorkloadPlacement, WorkloadTree,
};

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

impl AuthStore {
    // Every mutation locks the workspace row before reading the graph. This also
    // serializes simultaneous moves that would otherwise jointly form a cycle.
    async fn lock_workload_tree(
        &self,
        workspace: &str,
    ) -> Result<Transaction<'_, Postgres>, AuthFailure> {
        let mut tx = self.pool.begin().await.map_err(AuthFailure::database)?;
        sqlx::query("SELECT workspace_id FROM workspaces WHERE workspace_id=$1 AND deleted_at IS NULL FOR UPDATE")
            .bind(workspace).fetch_optional(&mut *tx).await.map_err(AuthFailure::database)?
            .ok_or_else(|| AuthFailure::not_found("workspace_not_found", "workspace does not exist"))?;
        Ok(tx)
    }

    pub async fn workload_tree(&self, workspace: &str) -> Result<WorkloadTree, AuthFailure> {
        let mut tx = self.lock_workload_tree(workspace).await?;
        let folders = sqlx::query("SELECT folder_id,name,parent_id FROM workload_folders WHERE workspace_id=$1 ORDER BY lower(name),folder_id")
            .bind(workspace).fetch_all(&mut *tx).await.map_err(AuthFailure::database)?.into_iter().map(folder).collect();
        let placements = sqlx::query("SELECT p.agent_id,p.folder_id,p.position FROM workload_placements p JOIN agent_credentials a ON a.agent_id=p.agent_id AND a.workspace_id=p.workspace_id WHERE p.workspace_id=$1 AND a.revoked_at IS NULL ORDER BY p.folder_id NULLS FIRST,p.position,p.agent_id")
            .bind(workspace).fetch_all(&mut *tx).await.map_err(AuthFailure::database)?.into_iter().map(|r| WorkloadPlacement {agent_id:r.get("agent_id"),folder_id:r.get("folder_id"),position:r.get("position")}).collect();
        tx.commit().await.map_err(AuthFailure::database)?;
        Ok(WorkloadTree {
            folders,
            placements,
        })
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
        sqlx::query("UPDATE workload_folders SET name=$3,parent_id=$4 WHERE workspace_id=$1 AND folder_id=$2").bind(workspace).bind(id).bind(&result.name).bind(&result.parent_id).execute(&mut *tx).await.map_err(AuthFailure::database)?;
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
        // Append promoted placements after existing siblings, preserving both orders.
        sqlx::query("UPDATE workload_placements SET position=position+(SELECT COALESCE(MAX(position)+1,0) FROM workload_placements WHERE workspace_id=$1 AND folder_id IS NOT DISTINCT FROM $3),folder_id=$3 WHERE workspace_id=$1 AND folder_id=$2").bind(workspace).bind(id).bind(&result.parent_id).execute(&mut *tx).await.map_err(AuthFailure::database)?;
        sqlx::query("DELETE FROM workload_folders WHERE workspace_id=$1 AND folder_id=$2")
            .bind(workspace)
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(AuthFailure::database)?;
        normalize(&mut tx, workspace).await?;
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
        tx.commit().await.map_err(AuthFailure::database)?;
        Ok(WorkloadPlacement {
            agent_id: agent.into(),
            folder_id: request.folder_id,
            position,
        })
    }
}

fn validate_folder_name(name: &str) -> Result<String, AuthFailure> {
    let name = validate_resource_name(name, "folder")?;
    if name.contains('/') || matches!(name.as_str(), "." | "..") {
        return Err(invalid("folder names cannot contain / or equal . or .."));
    }
    Ok(name)
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

    async fn setup() -> AuthStore {
        let store = AuthStore::for_test("password").await;
        store.seed_test_workspace("one").await;
        store.seed_test_workspace("two").await;
        for (id, ws) in [("a", "one"), ("b", "one"), ("c", "one"), ("foreign", "two")] {
            sqlx::query("INSERT INTO agent_credentials(agent_id,workspace_id,server_id,secret_hash,created_at) VALUES($1,$2,'machine','hash','now')").bind(id).bind(ws).execute(&store.pool).await.unwrap();
        }
        store
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
