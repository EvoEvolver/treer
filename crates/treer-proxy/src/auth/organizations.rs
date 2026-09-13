use super::*;

impl AuthStore {
    pub async fn all_workspaces(&self) -> Result<Vec<WorkspaceInfo>, AuthFailure> {
        let rows = sqlx::query(
            "SELECT workspace_id, name, created_at FROM workspaces \
             WHERE deleted_at IS NULL ORDER BY workspace_id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        rows.into_iter().map(workspace_from_row).collect()
    }

    pub async fn list_organizations(
        &self,
        user_id: &str,
    ) -> Result<Vec<OrganizationInfo>, AuthFailure> {
        let rows = if self.disabled {
            sqlx::query(
                "SELECT organization_id, name, created_at, 'owner' AS role \
                 FROM organizations ORDER BY lower(name), organization_id",
            )
            .fetch_all(&self.pool)
            .await
        } else {
            sqlx::query(
                "SELECT o.organization_id, o.name, o.created_at, m.role \
                 FROM organizations o \
                 JOIN organization_members m ON m.organization_id = o.organization_id \
                 WHERE m.user_id = $1 \
                 ORDER BY lower(o.name), o.organization_id",
            )
            .bind(user_id)
            .fetch_all(&self.pool)
            .await
        }
        .map_err(AuthFailure::database)?;
        Ok(rows
            .into_iter()
            .map(|row| OrganizationInfo {
                organization_id: row.get("organization_id"),
                name: row.get("name"),
                role: row.get("role"),
                created_at: row.get("created_at"),
            })
            .collect())
    }

    pub async fn create_organization(
        &self,
        user_id: &str,
        name: &str,
    ) -> Result<OrganizationInfo, AuthFailure> {
        let name = validate_resource_name(name, "organization")?;
        let organization_id = format!("org_{}", Uuid::new_v4().simple());
        let now = Utc::now().to_rfc3339();
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        sqlx::query(
            "INSERT INTO organizations(organization_id, name, created_at, created_by) \
             VALUES($1, $2, $3, $4)",
        )
        .bind(&organization_id)
        .bind(&name)
        .bind(&now)
        .bind(user_id)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        sqlx::query(
            "INSERT INTO organization_members(organization_id, user_id, role, joined_at) \
             SELECT $1, id, 'owner', $2 FROM users WHERE id = $3",
        )
        .bind(&organization_id)
        .bind(&now)
        .bind(user_id)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        audit::insert(
            &mut transaction,
            NewAuditEvent {
                organization_id: &organization_id,
                workspace_id: None,
                actor_kind: "user",
                actor_id: Some(user_id),
                source: "api",
                action: "organization.created",
                resource_kind: "organization",
                resource_id: &organization_id,
                resource_name: Some(&name),
                payload: json!({}),
            },
        )
        .await
        .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        Ok(OrganizationInfo {
            organization_id,
            name,
            role: "owner".to_string(),
            created_at: now,
        })
    }

    pub async fn rename_organization(
        &self,
        organization_id: &str,
        user_id: &str,
        name: &str,
    ) -> Result<OrganizationInfo, AuthFailure> {
        let role = self.require_manager(organization_id, user_id).await?;
        let name = validate_resource_name(name, "organization")?;
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        let old_name = sqlx::query_scalar::<_, String>(
            "SELECT name FROM organizations WHERE organization_id = $1 FOR UPDATE",
        )
        .bind(organization_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?
        .ok_or_else(|| {
            AuthFailure::not_found("organization_not_found", "organization does not exist")
        })?;
        let result = sqlx::query("UPDATE organizations SET name = $1 WHERE organization_id = $2")
            .bind(&name)
            .bind(organization_id)
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        if result.rows_affected() != 1 {
            return Err(AuthFailure::not_found(
                "organization_not_found",
                "organization does not exist",
            ));
        }
        let row = sqlx::query("SELECT created_at FROM organizations WHERE organization_id = $1")
            .bind(organization_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        audit::insert(
            &mut transaction,
            NewAuditEvent {
                organization_id,
                workspace_id: None,
                actor_kind: "user",
                actor_id: Some(user_id),
                source: "api",
                action: "organization.renamed",
                resource_kind: "organization",
                resource_id: organization_id,
                resource_name: Some(&name),
                payload: json!({ "old_name": old_name, "new_name": name }),
            },
        )
        .await
        .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        Ok(OrganizationInfo {
            organization_id: organization_id.to_string(),
            name,
            role,
            created_at: row.get("created_at"),
        })
    }

    pub async fn active_machine_count(&self) -> Result<i64, AuthFailure> {
        sqlx::query_scalar("SELECT COUNT(*) FROM machines WHERE revoked_at IS NULL")
            .fetch_one(&self.pool)
            .await
            .map_err(AuthFailure::database)
    }

    pub async fn user_count(&self) -> Result<i64, AuthFailure> {
        sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(&self.pool)
            .await
            .map_err(AuthFailure::database)
    }

    pub async fn organization_count(&self) -> Result<i64, AuthFailure> {
        sqlx::query_scalar("SELECT COUNT(*) FROM organizations")
            .fetch_one(&self.pool)
            .await
            .map_err(AuthFailure::database)
    }

    pub async fn require_organization_member(
        &self,
        organization_id: &str,
        user_id: &str,
    ) -> Result<String, AuthFailure> {
        self.membership_role(organization_id, user_id)
            .await?
            .ok_or_else(|| {
                AuthFailure::forbidden(
                    "organization_access_denied",
                    "you are not a member of this organization",
                )
            })
    }

    pub async fn require_workspace_member(
        &self,
        workspace_id: &str,
        user_id: &str,
    ) -> Result<(), AuthFailure> {
        self.workspace_member_role(workspace_id, user_id).await?;
        Ok(())
    }

    pub async fn workspace_member_role(
        &self,
        workspace_id: &str,
        user_id: &str,
    ) -> Result<String, AuthFailure> {
        if self.disabled {
            let exists = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM workspaces WHERE workspace_id = $1 AND deleted_at IS NULL)",
            )
            .bind(workspace_id)
            .fetch_one(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
            return exists.then(|| "owner".to_string()).ok_or_else(|| {
                AuthFailure::not_found("workspace_not_found", "workspace does not exist")
            });
        }
        let role = sqlx::query_scalar::<_, Option<String>>(
            "SELECT CASE \
                 WHEN om.role IN ('owner', 'admin') THEN 'owner' \
                 WHEN ug.role = 'owner' OR EXISTS( \
                   SELECT 1 FROM workspace_group_grants wg \
                   JOIN organization_group_members gm ON gm.group_id = wg.group_id \
                   WHERE wg.workspace_id = w.workspace_id AND gm.user_id = $2 AND wg.role = 'owner' \
                 ) THEN 'owner' \
                 WHEN w.access_mode = 'organization' OR ug.user_id IS NOT NULL OR EXISTS( \
                   SELECT 1 FROM workspace_group_grants wg \
                   JOIN organization_group_members gm ON gm.group_id = wg.group_id \
                   WHERE wg.workspace_id = w.workspace_id AND gm.user_id = $2 \
                 ) THEN 'member' \
                 ELSE NULL END \
             FROM workspaces w \
             JOIN organization_members om ON om.organization_id = w.organization_id AND om.user_id = $2 \
             LEFT JOIN workspace_user_grants ug ON ug.workspace_id = w.workspace_id AND ug.user_id = $2 \
             WHERE w.workspace_id = $1 AND w.deleted_at IS NULL",
        )
        .bind(workspace_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)?
        .flatten();
        if let Some(role) = role {
            return Ok(role);
        }
        let exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM workspaces WHERE workspace_id = $1 AND deleted_at IS NULL)",
        )
        .bind(workspace_id)
        .fetch_one(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        if exists {
            Err(AuthFailure::forbidden(
                "workspace_access_denied",
                "you do not have access to this workspace",
            ))
        } else {
            Err(AuthFailure::not_found(
                "workspace_not_found",
                "workspace does not exist",
            ))
        }
    }

    pub async fn require_workspace_owner(
        &self,
        workspace_id: &str,
        user_id: &str,
    ) -> Result<(), AuthFailure> {
        if self.workspace_member_role(workspace_id, user_id).await? == "owner" {
            Ok(())
        } else {
            Err(AuthFailure::forbidden(
                "workspace_owner_required",
                "workspace owner access required",
            ))
        }
    }

    pub async fn list_members(
        &self,
        organization_id: &str,
        user_id: &str,
    ) -> Result<Vec<OrganizationMember>, AuthFailure> {
        self.require_organization_member(organization_id, user_id)
            .await?;
        let rows = sqlx::query(
            "SELECT u.id AS user_id, u.email, u.preferred_name, m.role, m.joined_at \
             FROM organization_members m JOIN users u ON u.id = m.user_id \
             WHERE m.organization_id = $1 \
             ORDER BY CASE m.role WHEN 'owner' THEN 0 WHEN 'admin' THEN 1 ELSE 2 END, \
             lower(u.preferred_name), lower(u.email)",
        )
        .bind(organization_id)
        .fetch_all(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        Ok(rows
            .into_iter()
            .map(|row| OrganizationMember {
                user_id: row.get("user_id"),
                email: row.get("email"),
                preferred_name: row.get("preferred_name"),
                role: row.get("role"),
                joined_at: row.get("joined_at"),
            })
            .collect())
    }

    pub async fn list_workspace_humans(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<WorkspaceHuman>, AuthFailure> {
        let rows = sqlx::query(
            "SELECT u.id AS user_id, u.preferred_name, CASE \
               WHEN m.role IN ('owner', 'admin') OR ug.role = 'owner' OR EXISTS( \
                 SELECT 1 FROM workspace_group_grants wg \
                 JOIN organization_group_members gm ON gm.group_id = wg.group_id \
                 WHERE wg.workspace_id = w.workspace_id AND gm.user_id = u.id AND wg.role = 'owner' \
               ) THEN 'owner' ELSE 'member' END AS role \
             FROM workspaces w \
             JOIN organization_members m ON m.organization_id = w.organization_id \
             JOIN users u ON u.id = m.user_id \
             LEFT JOIN workspace_user_grants ug ON ug.workspace_id = w.workspace_id AND ug.user_id = u.id \
             WHERE w.workspace_id = $1 AND w.deleted_at IS NULL AND ( \
               w.access_mode = 'organization' OR m.role IN ('owner', 'admin') OR ug.user_id IS NOT NULL OR EXISTS( \
                 SELECT 1 FROM workspace_group_grants wg \
                 JOIN organization_group_members gm ON gm.group_id = wg.group_id \
                 WHERE wg.workspace_id = w.workspace_id AND gm.user_id = u.id \
               ) \
             ) \
             ORDER BY CASE WHEN m.role IN ('owner', 'admin') OR ug.role = 'owner' THEN 0 ELSE 1 END, \
                      lower(u.preferred_name), u.id",
        )
        .bind(workspace_id)
        .fetch_all(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        Ok(rows
            .into_iter()
            .map(|row| WorkspaceHuman {
                user_id: row.get("user_id"),
                preferred_name: row.get("preferred_name"),
                role: row.get("role"),
            })
            .collect())
    }

    pub async fn list_workspaces(
        &self,
        organization_id: &str,
        user_id: &str,
    ) -> Result<Vec<WorkspaceInfo>, AuthFailure> {
        self.require_organization_member(organization_id, user_id)
            .await?;
        let rows = sqlx::query(
            "SELECT workspace_id, name, created_at FROM workspaces \
             WHERE organization_id = $1 AND deleted_at IS NULL AND ( \
               $3::BOOLEAN OR access_mode = 'organization' OR EXISTS( \
                 SELECT 1 FROM organization_members om \
                 WHERE om.organization_id = workspaces.organization_id AND om.user_id = $2 \
                   AND om.role IN ('owner', 'admin') \
               ) OR EXISTS( \
                 SELECT 1 FROM workspace_user_grants ug \
                 WHERE ug.workspace_id = workspaces.workspace_id AND ug.user_id = $2 \
               ) OR EXISTS( \
                 SELECT 1 FROM workspace_group_grants wg \
                 JOIN organization_group_members gm ON gm.group_id = wg.group_id \
                 WHERE wg.workspace_id = workspaces.workspace_id AND gm.user_id = $2 \
               ) \
             ) \
             ORDER BY lower(name), workspace_id",
        )
        .bind(organization_id)
        .bind(user_id)
        .bind(self.disabled)
        .fetch_all(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        rows.into_iter().map(workspace_from_row).collect()
    }

    pub async fn workspace_access_info(
        &self,
        workspace_id: &str,
        user_id: &str,
    ) -> Result<WorkspaceAccessInfo, AuthFailure> {
        let current_role = self.workspace_member_role(workspace_id, user_id).await?;
        let access_mode = sqlx::query_scalar::<_, String>(
            "SELECT access_mode FROM workspaces WHERE workspace_id = $1 AND deleted_at IS NULL",
        )
        .bind(workspace_id)
        .fetch_one(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        let members = sqlx::query(
            "SELECT u.id AS user_id, u.preferred_name, u.email, g.role \
             FROM workspace_user_grants g \
             JOIN workspaces w ON w.workspace_id = g.workspace_id \
             JOIN organization_members om ON om.organization_id = w.organization_id AND om.user_id = g.user_id \
             JOIN users u ON u.id = g.user_id \
             WHERE g.workspace_id = $1 \
             ORDER BY CASE g.role WHEN 'owner' THEN 0 ELSE 1 END, lower(u.preferred_name), u.id",
        )
        .bind(workspace_id)
        .fetch_all(&self.pool)
        .await
        .map_err(AuthFailure::database)?
        .into_iter()
        .map(|row| WorkspaceAccessMember {
            user_id: row.get("user_id"),
            preferred_name: row.get("preferred_name"),
            email: row.get("email"),
            role: row.get("role"),
        })
        .collect();
        let groups = sqlx::query(
            "SELECT og.group_id, og.name, wg.role, COUNT(gm.user_id) AS member_count \
             FROM workspace_group_grants wg \
             JOIN organization_groups og ON og.group_id = wg.group_id \
             LEFT JOIN organization_group_members gm ON gm.group_id = og.group_id \
             WHERE wg.workspace_id = $1 \
             GROUP BY og.group_id, og.name, wg.role \
             ORDER BY CASE wg.role WHEN 'owner' THEN 0 ELSE 1 END, lower(og.name), og.group_id",
        )
        .bind(workspace_id)
        .fetch_all(&self.pool)
        .await
        .map_err(AuthFailure::database)?
        .into_iter()
        .map(|row| WorkspaceAccessGroup {
            group_id: row.get("group_id"),
            name: row.get("name"),
            role: row.get("role"),
            member_count: row.get("member_count"),
        })
        .collect();
        Ok(WorkspaceAccessInfo {
            workspace_id: workspace_id.to_string(),
            access_mode,
            current_role,
            members,
            groups,
        })
    }

    pub async fn update_workspace_access_mode(
        &self,
        workspace_id: &str,
        user_id: &str,
        access_mode: &str,
    ) -> Result<WorkspaceAccessInfo, AuthFailure> {
        self.require_workspace_owner(workspace_id, user_id).await?;
        if !matches!(access_mode, "organization" | "restricted") {
            return Err(AuthFailure::bad_request(
                "invalid_workspace_access_mode",
                "workspace access mode must be organization or restricted",
            ));
        }
        sqlx::query(
            "UPDATE workspaces SET access_mode = $1 WHERE workspace_id = $2 AND deleted_at IS NULL",
        )
        .bind(access_mode)
        .bind(workspace_id)
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        self.workspace_access_info(workspace_id, user_id).await
    }

    pub async fn upsert_workspace_user_grant(
        &self,
        workspace_id: &str,
        actor: &str,
        target_user_id: &str,
        role: &str,
    ) -> Result<WorkspaceAccessInfo, AuthFailure> {
        self.require_workspace_owner(workspace_id, actor).await?;
        validate_workspace_role(role)?;
        if actor == target_user_id && role != "owner" {
            return Err(AuthFailure::conflict(
                "workspace_owner_cannot_demote_self",
                "add another owner before changing your own workspace role",
            ));
        }
        let belongs = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM workspaces w JOIN organization_members om \
             ON om.organization_id = w.organization_id \
             WHERE w.workspace_id = $1 AND om.user_id = $2 AND w.deleted_at IS NULL)",
        )
        .bind(workspace_id)
        .bind(target_user_id)
        .fetch_one(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        if !belongs {
            return Err(AuthFailure::bad_request(
                "workspace_member_must_belong_to_organization",
                "workspace members must belong to the organization",
            ));
        }
        sqlx::query(
            "INSERT INTO workspace_user_grants(workspace_id, user_id, role, created_at, created_by) \
             VALUES($1, $2, $3, $4, $5) ON CONFLICT(workspace_id, user_id) DO UPDATE SET role = excluded.role",
        )
        .bind(workspace_id)
        .bind(target_user_id)
        .bind(role)
        .bind(Utc::now().to_rfc3339())
        .bind(actor)
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        self.workspace_access_info(workspace_id, actor).await
    }

    pub async fn remove_workspace_user_grant(
        &self,
        workspace_id: &str,
        actor: &str,
        target_user_id: &str,
    ) -> Result<WorkspaceAccessInfo, AuthFailure> {
        self.require_workspace_owner(workspace_id, actor).await?;
        if actor == target_user_id {
            return Err(AuthFailure::conflict(
                "workspace_owner_cannot_remove_self",
                "another workspace owner must remove your access",
            ));
        }
        sqlx::query("DELETE FROM workspace_user_grants WHERE workspace_id = $1 AND user_id = $2")
            .bind(workspace_id)
            .bind(target_user_id)
            .execute(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
        self.workspace_access_info(workspace_id, actor).await
    }

    pub async fn upsert_workspace_group_grant(
        &self,
        workspace_id: &str,
        actor: &str,
        group_id: &str,
        role: &str,
    ) -> Result<WorkspaceAccessInfo, AuthFailure> {
        self.require_workspace_owner(workspace_id, actor).await?;
        validate_workspace_role(role)?;
        let same_organization = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM workspaces w JOIN organization_groups g \
             ON g.organization_id = w.organization_id \
             WHERE w.workspace_id = $1 AND g.group_id = $2 AND w.deleted_at IS NULL)",
        )
        .bind(workspace_id)
        .bind(group_id)
        .fetch_one(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        if !same_organization {
            return Err(AuthFailure::bad_request(
                "workspace_group_must_belong_to_organization",
                "workspace groups must belong to the organization",
            ));
        }
        sqlx::query(
            "INSERT INTO workspace_group_grants(workspace_id, group_id, role, created_at, created_by) \
             VALUES($1, $2, $3, $4, $5) ON CONFLICT(workspace_id, group_id) DO UPDATE SET role = excluded.role",
        )
        .bind(workspace_id)
        .bind(group_id)
        .bind(role)
        .bind(Utc::now().to_rfc3339())
        .bind(actor)
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        self.workspace_access_info(workspace_id, actor).await
    }

    pub async fn remove_workspace_group_grant(
        &self,
        workspace_id: &str,
        actor: &str,
        group_id: &str,
    ) -> Result<WorkspaceAccessInfo, AuthFailure> {
        self.require_workspace_owner(workspace_id, actor).await?;
        sqlx::query("DELETE FROM workspace_group_grants WHERE workspace_id = $1 AND group_id = $2")
            .bind(workspace_id)
            .bind(group_id)
            .execute(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
        self.workspace_access_info(workspace_id, actor).await
    }

    pub async fn list_organization_groups(
        &self,
        organization_id: &str,
        user_id: &str,
    ) -> Result<Vec<OrganizationGroup>, AuthFailure> {
        self.require_organization_member(organization_id, user_id)
            .await?;
        let rows = sqlx::query(
            "SELECT group_id, organization_id, name FROM organization_groups \
             WHERE organization_id = $1 ORDER BY lower(name), group_id",
        )
        .bind(organization_id)
        .fetch_all(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        let mut groups = Vec::with_capacity(rows.len());
        for row in rows {
            let group_id: String = row.get("group_id");
            let member_ids = sqlx::query_scalar::<_, String>(
                "SELECT user_id FROM organization_group_members WHERE group_id = $1 ORDER BY user_id",
            )
            .bind(&group_id)
            .fetch_all(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
            groups.push(OrganizationGroup {
                group_id,
                organization_id: row.get("organization_id"),
                name: row.get("name"),
                member_ids,
            });
        }
        Ok(groups)
    }

    pub async fn create_organization_group(
        &self,
        organization_id: &str,
        actor: &str,
        name: &str,
    ) -> Result<OrganizationGroup, AuthFailure> {
        self.require_manager(organization_id, actor).await?;
        let name = validate_resource_name(name, "group")?;
        let group = OrganizationGroup {
            group_id: format!("grp_{}", Uuid::new_v4().simple()),
            organization_id: organization_id.to_string(),
            name,
            member_ids: Vec::new(),
        };
        sqlx::query(
            "INSERT INTO organization_groups(group_id, organization_id, name, created_at, created_by) \
             VALUES($1, $2, $3, $4, $5)",
        )
        .bind(&group.group_id)
        .bind(organization_id)
        .bind(&group.name)
        .bind(Utc::now().to_rfc3339())
        .bind(actor)
        .execute(&self.pool)
        .await
        .map_err(|error| {
            if error
                .as_database_error()
                .is_some_and(|error| error.is_unique_violation())
            {
                AuthFailure::conflict("group_exists", "group name already exists")
            } else {
                AuthFailure::database(error)
            }
        })?;
        Ok(group)
    }

    pub async fn delete_organization_group(
        &self,
        organization_id: &str,
        actor: &str,
        group_id: &str,
    ) -> Result<(), AuthFailure> {
        self.require_manager(organization_id, actor).await?;
        let result = sqlx::query(
            "DELETE FROM organization_groups WHERE organization_id = $1 AND group_id = $2",
        )
        .bind(organization_id)
        .bind(group_id)
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        if result.rows_affected() == 0 {
            return Err(AuthFailure::not_found(
                "group_not_found",
                "group does not exist",
            ));
        }
        Ok(())
    }

    pub async fn set_organization_group_member(
        &self,
        organization_id: &str,
        actor: &str,
        group_id: &str,
        target_user_id: &str,
        present: bool,
    ) -> Result<Vec<OrganizationGroup>, AuthFailure> {
        self.require_manager(organization_id, actor).await?;
        let valid = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM organization_groups g JOIN organization_members om \
             ON om.organization_id = g.organization_id \
             WHERE g.organization_id = $1 AND g.group_id = $2 AND om.user_id = $3)",
        )
        .bind(organization_id)
        .bind(group_id)
        .bind(target_user_id)
        .fetch_one(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        if !valid {
            return Err(AuthFailure::bad_request(
                "invalid_group_member",
                "the group and member must belong to the organization",
            ));
        }
        if present {
            sqlx::query(
                "INSERT INTO organization_group_members(group_id, user_id, added_at, added_by) \
                 VALUES($1, $2, $3, $4) ON CONFLICT(group_id, user_id) DO NOTHING",
            )
            .bind(group_id)
            .bind(target_user_id)
            .bind(Utc::now().to_rfc3339())
            .bind(actor)
            .execute(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
        } else {
            sqlx::query(
                "DELETE FROM organization_group_members WHERE group_id = $1 AND user_id = $2",
            )
            .bind(group_id)
            .bind(target_user_id)
            .execute(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
        }
        self.list_organization_groups(organization_id, actor).await
    }

    pub async fn create_workspace(
        &self,
        organization_id: &str,
        workspace_id: &str,
        name: &str,
        user_id: &str,
    ) -> Result<WorkspaceInfo, AuthFailure> {
        self.require_organization_member(organization_id, user_id)
            .await?;
        let name = validate_resource_name(name, "workspace")?;
        let now = Utc::now();
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        sqlx::query(
            "INSERT INTO workspaces(workspace_id, organization_id, name, created_at, created_by) \
             VALUES($1, $2, $3, $4, $5)",
        )
        .bind(workspace_id)
        .bind(organization_id)
        .bind(&name)
        .bind(now.to_rfc3339())
        .bind(user_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| {
            if error
                .as_database_error()
                .is_some_and(|error| error.is_unique_violation())
            {
                AuthFailure::conflict("workspace_exists", "workspace already exists")
            } else {
                AuthFailure::database(error)
            }
        })?;
        sqlx::query(
            "INSERT INTO workspace_user_grants(workspace_id, user_id, role, created_at, created_by) \
             SELECT $1, id, 'owner', $2, $3 FROM users WHERE id = $3",
        )
        .bind(workspace_id)
        .bind(now.to_rfc3339())
        .bind(user_id)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        insert_default_agent_launch_profiles(&mut transaction, workspace_id, user_id, &now).await?;
        audit::insert(
            &mut transaction,
            NewAuditEvent {
                organization_id,
                workspace_id: Some(workspace_id),
                actor_kind: "user",
                actor_id: Some(user_id),
                source: "api",
                action: "workspace.created",
                resource_kind: "workspace",
                resource_id: workspace_id,
                resource_name: Some(&name),
                payload: json!({}),
            },
        )
        .await
        .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        Ok(WorkspaceInfo {
            workspace_id: workspace_id.to_string(),
            name,
            created_at: now,
        })
    }

    pub async fn rename_workspace(
        &self,
        workspace_id: &str,
        user_id: &str,
        name: &str,
    ) -> Result<WorkspaceInfo, AuthFailure> {
        self.require_workspace_member(workspace_id, user_id).await?;
        let name = validate_resource_name(name, "workspace")?;
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        let row = sqlx::query(
            "SELECT organization_id, name FROM workspaces \
             WHERE workspace_id = $1 AND deleted_at IS NULL FOR UPDATE",
        )
        .bind(workspace_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?
        .ok_or_else(|| AuthFailure::not_found("workspace_not_found", "workspace does not exist"))?;
        let organization_id: String = row.get("organization_id");
        let old_name: String = row.get("name");
        sqlx::query(
            "UPDATE workspaces SET name = $1 \
             WHERE workspace_id = $2 AND deleted_at IS NULL",
        )
        .bind(&name)
        .bind(workspace_id)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        audit::insert(
            &mut transaction,
            NewAuditEvent {
                organization_id: &organization_id,
                workspace_id: Some(workspace_id),
                actor_kind: "user",
                actor_id: Some(user_id),
                source: "api",
                action: "workspace.renamed",
                resource_kind: "workspace",
                resource_id: workspace_id,
                resource_name: Some(&name),
                payload: json!({ "old_name": old_name, "new_name": name }),
            },
        )
        .await
        .map_err(AuthFailure::database)?;
        let info = workspace_from_row(
            sqlx::query(
                "SELECT workspace_id, name, created_at FROM workspaces \
                 WHERE workspace_id = $1 AND deleted_at IS NULL",
            )
            .bind(workspace_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?,
        )?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        Ok(info)
    }

    pub async fn delete_workspace(
        &self,
        workspace_id: &str,
        user_id: &str,
    ) -> Result<DeletedWorkspace, AuthFailure> {
        self.require_workspace_owner(workspace_id, user_id).await?;
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        let row = sqlx::query(
            "SELECT organization_id, name FROM workspaces \
             WHERE workspace_id = $1 AND deleted_at IS NULL FOR UPDATE",
        )
        .bind(workspace_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?
        .ok_or_else(|| AuthFailure::not_found("workspace_not_found", "workspace does not exist"))?;
        let organization_id: String = row.get("organization_id");
        let name: String = row.get("name");
        let machine_count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM machines WHERE workspace_id = $1 AND revoked_at IS NULL",
        )
        .bind(workspace_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        if machine_count != 0 {
            return Err(AuthFailure::conflict(
                "workspace_has_machines",
                "delete all machines in this workspace first",
            ));
        }
        let agent_count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM agent_credentials WHERE workspace_id = $1",
        )
        .bind(workspace_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        let app_count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM app_deployments WHERE workspace_id = $1",
        )
        .bind(workspace_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "UPDATE agent_credentials SET revoked_at = $1 \
             WHERE workspace_id = $2 AND revoked_at IS NULL",
        )
        .bind(&now)
        .bind(workspace_id)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        audit::insert(
            &mut transaction,
            NewAuditEvent {
                organization_id: &organization_id,
                workspace_id: Some(workspace_id),
                actor_kind: "user",
                actor_id: Some(user_id),
                source: "api",
                action: "workspace.deleted",
                resource_kind: "workspace",
                resource_id: workspace_id,
                resource_name: Some(&name),
                payload: json!({
                    "machine_count": machine_count,
                    "agent_count": agent_count,
                    "app_count": app_count,
                }),
            },
        )
        .await
        .map_err(AuthFailure::database)?;
        sqlx::query(
            "UPDATE workspaces SET deleted_at = $1, deleted_by = $2 \
             WHERE workspace_id = $3 AND deleted_at IS NULL",
        )
        .bind(&now)
        .bind(user_id)
        .bind(workspace_id)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        self.agent_credentials
            .write()
            .await
            .retain(|_, record| record.workspace_id != workspace_id);
        let _update = self.service_ingresses_update.lock().await;
        self.service_ingresses
            .write()
            .await
            .retain(|_, resolved| resolved.ingress.workspace_id != workspace_id);
        if self
            .virtual_hosts
            .write()
            .await
            .remove(workspace_id)
            .is_some()
        {
            self.virtual_hosts_revision.fetch_add(1, Ordering::SeqCst);
        }
        Ok(DeletedWorkspace {
            workspace_id: workspace_id.to_string(),
            organization_id,
            name,
            machine_count,
            agent_count,
            app_count,
        })
    }
}
