//! Organization/workspace administration shared by human sessions and Agents.
//!
//! Humans keep their organization and workspace role checks. An Agent reaches
//! these methods after the API layer authenticated its workload credential
//! and evaluated its workspace Policy like any other action. Agents are
//! trusted collaborators: here an Agent acts like an owner of its own
//! workspace and of that workspace's organization, never outside them. The
//! same invariants (last owner, membership, scope) apply to both.
use super::*;
use treer_protocol::{
    AccessVia, AccessViaKind, ActorSummary, AuthSessionInfo, CreateInvitationRequest,
    CreatedInvitation, DeleteWorkspaceRequest, EffectiveAccessMember, EffectiveWorkspaceAccess,
    InvitationStatus, InvitationSummary, OrganizationRole, UserSummary,
    INVITATION_DEFAULT_EXPIRES_HOURS, INVITATION_MAX_EXPIRES_HOURS,
};

const MAX_INVITATION_GRANTS: usize = 64;
const SESSION_LAST_SEEN_RESOLUTION_SECONDS: i64 = 300;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdminActor {
    User(String),
    /// An authenticated Agent; scoped to its own workspace and organization.
    Agent {
        agent_id: String,
        workspace_id: String,
    },
}

impl AdminActor {
    pub fn user(user_id: &str) -> Self {
        Self::User(user_id.to_string())
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::User(_) => "user",
            Self::Agent { .. } => "agent",
        }
    }

    pub fn id(&self) -> &str {
        match self {
            Self::User(id) => id,
            Self::Agent { agent_id, .. } => agent_id,
        }
    }

    /// Value for `created_by`-style text columns, which hold user IDs; Agent
    /// writes are prefixed so they can never be mistaken for a user.
    pub fn label(&self) -> String {
        match self {
            Self::User(id) => id.clone(),
            Self::Agent { agent_id, .. } => format!("agent:{agent_id}"),
        }
    }

    pub fn user_id(&self) -> Option<&str> {
        match self {
            Self::User(id) => Some(id),
            Self::Agent { .. } => None,
        }
    }
}

pub(crate) struct DeletedOrganizationBlocked {
    pub workspace_count: i64,
}

fn agent_scope_denied() -> AuthFailure {
    AuthFailure::forbidden(
        "agent_scope_denied",
        "Agents administer only their own workspace and its organization",
    )
}

fn last_owner() -> AuthFailure {
    AuthFailure::conflict("last_owner", "an organization must keep at least one owner")
}

impl AuthStore {
    pub(crate) async fn workspace_organization(
        &self,
        workspace_id: &str,
    ) -> Result<String, AuthFailure> {
        sqlx::query_scalar(
            "SELECT organization_id FROM workspaces WHERE workspace_id = $1 AND deleted_at IS NULL",
        )
        .bind(workspace_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)?
        .ok_or_else(|| AuthFailure::not_found("workspace_not_found", "workspace does not exist"))
    }

    async fn require_agent_organization(
        &self,
        agent_workspace: &str,
        organization_id: &str,
    ) -> Result<(), AuthFailure> {
        if self.workspace_organization(agent_workspace).await? == organization_id {
            Ok(())
        } else {
            Err(agent_scope_denied())
        }
    }

    fn require_agent_workspace(actor: &AdminActor, workspace_id: &str) -> Result<(), AuthFailure> {
        match actor {
            AdminActor::Agent {
                workspace_id: own, ..
            } if own != workspace_id => Err(agent_scope_denied()),
            _ => Ok(()),
        }
    }

    /// Any organization member (humans); an Agent of the organization.
    pub(super) async fn require_member_as(
        &self,
        organization_id: &str,
        actor: &AdminActor,
    ) -> Result<String, AuthFailure> {
        match actor {
            AdminActor::User(user_id) => {
                self.require_organization_member(organization_id, user_id)
                    .await
            }
            AdminActor::Agent { workspace_id, .. } => {
                self.require_agent_organization(workspace_id, organization_id)
                    .await?;
                Ok("agent".into())
            }
        }
    }

    /// Owner or admin (humans); an Agent of the organization.
    pub(super) async fn require_manager_as(
        &self,
        organization_id: &str,
        actor: &AdminActor,
    ) -> Result<String, AuthFailure> {
        match actor {
            AdminActor::User(user_id) => self.require_manager(organization_id, user_id).await,
            AdminActor::Agent { .. } => self.require_member_as(organization_id, actor).await,
        }
    }

    /// Owner (humans); an Agent of the organization.
    pub(super) async fn require_owner_as(
        &self,
        organization_id: &str,
        actor: &AdminActor,
    ) -> Result<String, AuthFailure> {
        let role = self.require_member_as(organization_id, actor).await?;
        if matches!(role.as_str(), "owner" | "agent") {
            Ok(role)
        } else {
            Err(AuthFailure::forbidden(
                "organization_owner_required",
                "organization owner access required",
            ))
        }
    }

    /// Workspace role of a human; Agents only in their own workspace.
    pub(super) async fn workspace_role_as(
        &self,
        workspace_id: &str,
        actor: &AdminActor,
    ) -> Result<String, AuthFailure> {
        match actor {
            AdminActor::User(user_id) => self.workspace_member_role(workspace_id, user_id).await,
            AdminActor::Agent { .. } => {
                Self::require_agent_workspace(actor, workspace_id)?;
                self.workspace_organization(workspace_id).await?;
                Ok("agent".into())
            }
        }
    }

    pub(super) async fn require_workspace_owner_as(
        &self,
        workspace_id: &str,
        actor: &AdminActor,
    ) -> Result<(), AuthFailure> {
        if matches!(
            self.workspace_role_as(workspace_id, actor).await?.as_str(),
            "owner" | "agent"
        ) {
            Ok(())
        } else {
            Err(AuthFailure::forbidden(
                "workspace_owner_required",
                "workspace owner access required",
            ))
        }
    }

    pub(crate) async fn actor_summary(&self, actor: &AdminActor) -> ActorSummary {
        let name: Option<String> = match actor {
            AdminActor::User(id) => {
                sqlx::query_scalar("SELECT preferred_name FROM users WHERE id=$1")
                    .bind(id)
                    .fetch_optional(&self.pool)
                    .await
                    .ok()
                    .flatten()
            }
            AdminActor::Agent { agent_id, .. } => {
                sqlx::query_scalar("SELECT name FROM agent_names WHERE agent_id=$1")
                    .bind(agent_id)
                    .fetch_optional(&self.pool)
                    .await
                    .ok()
                    .flatten()
            }
        };
        ActorSummary {
            id: actor.id().to_string(),
            name: name.unwrap_or_else(|| actor.id().to_string()),
            kind: actor.kind().to_string(),
        }
    }

    // ---- Invitations ------------------------------------------------------

    pub(crate) async fn create_organization_invitation(
        &self,
        organization_id: &str,
        actor: &AdminActor,
        request: CreateInvitationRequest,
    ) -> Result<CreatedInvitation, AuthFailure> {
        self.insert_organization_invitation(organization_id, actor, request, true)
            .await
    }

    pub(super) async fn insert_organization_invitation(
        &self,
        organization_id: &str,
        actor: &AdminActor,
        request: CreateInvitationRequest,
        explicit_role: bool,
    ) -> Result<CreatedInvitation, AuthFailure> {
        let actor_role = self.require_manager_as(organization_id, actor).await?;
        if request.role == OrganizationRole::Owner && actor_role == "admin" {
            return Err(AuthFailure::forbidden(
                "organization_owner_required",
                "only an organization owner can invite an owner",
            ));
        }
        let hours = request
            .expires_in_hours
            .unwrap_or(INVITATION_DEFAULT_EXPIRES_HOURS);
        if !(1..=INVITATION_MAX_EXPIRES_HOURS).contains(&hours) {
            return Err(AuthFailure::bad_request(
                "invalid_invitation_expiry",
                "expires_in_hours must be between 1 and 720",
            ));
        }
        let email = request
            .email
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(normalize_email)
            .transpose()?;
        let group_ids = dedupe_ids(&request.group_ids, "group_ids")?;
        let workspace_ids = dedupe_ids(&request.workspace_ids, "workspace_ids")?;
        let valid_groups: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM organization_groups WHERE organization_id = $1 AND group_id = ANY($2)",
        )
        .bind(organization_id)
        .bind(&group_ids)
        .fetch_one(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        if valid_groups != group_ids.len() as i64 {
            return Err(AuthFailure::bad_request(
                "invitation_group_not_found",
                "every group must belong to the organization",
            ));
        }
        let valid_workspaces: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM workspaces WHERE organization_id = $1 \
             AND workspace_id = ANY($2) AND deleted_at IS NULL",
        )
        .bind(organization_id)
        .bind(&workspace_ids)
        .fetch_one(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        if valid_workspaces != workspace_ids.len() as i64 {
            return Err(AuthFailure::bad_request(
                "invitation_workspace_not_found",
                "every workspace must be an active workspace of the organization",
            ));
        }
        let token = format!("inv_{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let invitation_id = format!("invt_{}", Uuid::new_v4().simple());
        let now = Utc::now();
        let expires_at = now + Duration::hours(i64::from(hours));
        let created_by = self.actor_summary(actor).await;
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        sqlx::query(
            "INSERT INTO invitations(token, invitation_id, created_at, created_by, created_by_kind, \
             kind, organization_id, role, email, expires_at, group_ids, workspace_ids, explicit_role, \
             created_by_name) \
             VALUES($1, $2, $3, $4, $5, 'organization', $6, $7, $8, $9::timestamptz, $10, $11, $12, $13)",
        )
        .bind(&token)
        .bind(&invitation_id)
        .bind(now.to_rfc3339())
        .bind(actor.id())
        .bind(actor.kind())
        .bind(organization_id)
        .bind(request.role.as_str())
        .bind(&email)
        .bind(expires_at.to_rfc3339())
        .bind(json!(group_ids))
        .bind(json!(workspace_ids))
        .bind(explicit_role)
        .bind(&created_by.name)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        audit::insert(
            &mut transaction,
            NewAuditEvent {
                organization_id,
                workspace_id: None,
                actor_kind: actor.kind(),
                actor_id: Some(actor.id()),
                source: "api",
                action: "invitation.created",
                resource_kind: "invitation",
                resource_id: &invitation_id,
                resource_name: None,
                payload: json!({
                    "role": request.role.as_str(),
                    "expires_in_hours": hours,
                    "has_email": email.is_some(),
                    "group_count": group_ids.len(),
                    "workspace_count": workspace_ids.len(),
                }),
            },
        )
        .await
        .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        let url = self.invitation_url(&token);
        let email_queued = match (&email, self.email_sender.clone()) {
            (Some(recipient), Some(sender)) => {
                let organization_name: String =
                    sqlx::query_scalar("SELECT name FROM organizations WHERE organization_id=$1")
                        .bind(organization_id)
                        .fetch_one(&self.pool)
                        .await
                        .map_err(AuthFailure::database)?;
                let recipient = recipient.clone();
                let url = url.clone();
                tokio::spawn(async move {
                    if let Err(error) = sender
                        .send_invitation(&recipient, &organization_name, &url)
                        .await
                    {
                        // Never log the URL: it carries the invitation secret.
                        tracing::warn!(%error, "failed to send organization invitation email");
                    }
                });
                true
            }
            _ => false,
        };
        Ok(CreatedInvitation {
            id: invitation_id,
            url: url.to_string(),
            email,
            role: request.role,
            expires_at,
            created_at: now,
            created_by,
            group_ids,
            workspace_ids,
            email_queued,
        })
    }

    pub(super) fn invitation_url(&self, token: &str) -> Url {
        let mut url = self.app_public_url.clone();
        url.set_path("/");
        url.query_pairs_mut().clear().append_pair("invite", token);
        url
    }

    pub(crate) async fn list_organization_invitations(
        &self,
        organization_id: &str,
        actor: &AdminActor,
    ) -> Result<Vec<InvitationSummary>, AuthFailure> {
        self.require_manager_as(organization_id, actor).await?;
        let rows = sqlx::query(
            "SELECT i.invitation_id, i.email, i.role, i.created_at, \
             (extract(epoch FROM i.expires_at) * 1000)::bigint AS expires_ms, \
             i.used_at, (i.revoked_at IS NOT NULL) AS revoked, i.created_by, i.created_by_kind, i.group_ids, i.workspace_ids, \
             (i.expires_at IS NOT NULL AND i.expires_at <= now()) AS expired, \
             COALESCE(i.created_by_name, CASE i.created_by_kind WHEN 'agent' THEN NULL \
                  ELSE (SELECT preferred_name FROM users u WHERE u.id = i.created_by) END) AS creator_name \
             FROM invitations i WHERE i.kind = 'organization' AND i.organization_id = $1 \
             ORDER BY i.created_at DESC, i.invitation_id LIMIT 500",
        )
        .bind(organization_id)
        .fetch_all(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        rows.into_iter()
            .map(|row| {
                let status = if row.get::<Option<String>, _>("used_at").is_some() {
                    InvitationStatus::Accepted
                } else if row.get::<bool, _>("revoked") {
                    InvitationStatus::Revoked
                } else if row.get::<bool, _>("expired") {
                    InvitationStatus::Expired
                } else {
                    InvitationStatus::Pending
                };
                let created_by: String = row.get("created_by");
                let role: String = row.get("role");
                Ok(InvitationSummary {
                    id: row.get("invitation_id"),
                    email: row.get("email"),
                    role: OrganizationRole::parse(&role).unwrap_or(OrganizationRole::Member),
                    status,
                    created_at: parse_rfc3339(&row.get::<String, _>("created_at")),
                    expires_at: row
                        .get::<Option<i64>, _>("expires_ms")
                        .and_then(chrono::DateTime::from_timestamp_millis),
                    created_by: ActorSummary {
                        name: row
                            .get::<Option<String>, _>("creator_name")
                            .unwrap_or_else(|| created_by.clone()),
                        id: created_by,
                        kind: row.get("created_by_kind"),
                    },
                    group_ids: serde_json::from_value(row.get("group_ids")).unwrap_or_default(),
                    workspace_ids: serde_json::from_value(row.get("workspace_ids"))
                        .unwrap_or_default(),
                })
            })
            .collect()
    }

    pub(crate) async fn revoke_organization_invitation(
        &self,
        organization_id: &str,
        actor: &AdminActor,
        invitation_id: &str,
    ) -> Result<(), AuthFailure> {
        self.require_manager_as(organization_id, actor).await?;
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        let row = sqlx::query(
            "SELECT used_at, (revoked_at IS NOT NULL) AS revoked FROM invitations \
             WHERE kind = 'organization' AND organization_id = $1 AND invitation_id = $2 FOR UPDATE",
        )
        .bind(organization_id)
        .bind(invitation_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?
        .ok_or_else(|| {
            AuthFailure::not_found("invitation_not_found", "invitation does not exist")
        })?;
        if row.get::<Option<String>, _>("used_at").is_some() {
            return Err(AuthFailure::conflict(
                "invitation_already_accepted",
                "an accepted invitation cannot be revoked; remove the member instead",
            ));
        }
        if row.get::<bool, _>("revoked") {
            return Ok(());
        }
        sqlx::query("UPDATE invitations SET revoked_at = now() WHERE invitation_id = $1")
            .bind(invitation_id)
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        audit::insert(
            &mut transaction,
            NewAuditEvent {
                organization_id,
                workspace_id: None,
                actor_kind: actor.kind(),
                actor_id: Some(actor.id()),
                source: "api",
                action: "invitation.revoked",
                resource_kind: "invitation",
                resource_id: invitation_id,
                resource_name: None,
                payload: json!({}),
            },
        )
        .await
        .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)
    }

    // ---- Organization lifecycle (humans) -----------------------------------

    pub(crate) async fn leave_organization(
        &self,
        organization_id: &str,
        user_id: &str,
    ) -> Result<(), AuthFailure> {
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        let roles = lock_member_roles(&mut transaction, organization_id).await?;
        let role = roles
            .iter()
            .find(|(id, _)| id == user_id)
            .map(|(_, role)| role.clone())
            .ok_or_else(|| {
                AuthFailure::forbidden(
                    "organization_access_denied",
                    "you are not a member of this organization",
                )
            })?;
        if role == "owner" && roles.iter().filter(|(_, r)| r == "owner").count() <= 1 {
            return Err(last_owner());
        }
        remove_member_rows(&mut transaction, organization_id, user_id).await?;
        audit::insert(
            &mut transaction,
            NewAuditEvent {
                organization_id,
                workspace_id: None,
                actor_kind: "user",
                actor_id: Some(user_id),
                source: "api",
                action: "member.left",
                resource_kind: "organization_member",
                resource_id: user_id,
                resource_name: None,
                payload: json!({ "role": role }),
            },
        )
        .await
        .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)
    }

    pub(crate) async fn transfer_organization(
        &self,
        organization_id: &str,
        user_id: &str,
        target_user_id: &str,
    ) -> Result<(), AuthFailure> {
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        let roles = lock_member_roles(&mut transaction, organization_id).await?;
        let role_of = |id: &str| {
            roles
                .iter()
                .find(|(member, _)| member == id)
                .map(|(_, role)| role.as_str())
        };
        match role_of(user_id) {
            Some("owner") => {}
            Some(_) => {
                return Err(AuthFailure::forbidden(
                    "organization_owner_required",
                    "organization owner access required",
                ))
            }
            None => {
                return Err(AuthFailure::forbidden(
                    "organization_access_denied",
                    "you are not a member of this organization",
                ))
            }
        }
        if target_user_id == user_id {
            return Err(AuthFailure::bad_request(
                "invalid_transfer_target",
                "choose another member to receive ownership",
            ));
        }
        let previous = role_of(target_user_id)
            .ok_or_else(|| AuthFailure::not_found("member_not_found", "member does not exist"))?
            .to_string();
        for (member, role) in [(target_user_id, "owner"), (user_id, "admin")] {
            sqlx::query(
                "UPDATE organization_members SET role = $1 WHERE organization_id = $2 AND user_id = $3",
            )
            .bind(role)
            .bind(organization_id)
            .bind(member)
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        }
        audit::insert(
            &mut transaction,
            NewAuditEvent {
                organization_id,
                workspace_id: None,
                actor_kind: "user",
                actor_id: Some(user_id),
                source: "api",
                action: "organization.ownership_transferred",
                resource_kind: "organization_member",
                resource_id: target_user_id,
                resource_name: None,
                payload: json!({ "previous_role": previous, "previous_owner_role": "admin" }),
            },
        )
        .await
        .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)
    }

    /// Ok(Err(..)) reports the active workspaces that block deletion.
    pub(crate) async fn delete_organization(
        &self,
        organization_id: &str,
        actor: &AdminActor,
        confirm_name: &str,
    ) -> Result<Result<(), DeletedOrganizationBlocked>, AuthFailure> {
        self.require_owner_as(organization_id, actor).await?;
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        let name: String = sqlx::query_scalar(
            "SELECT name FROM organizations WHERE organization_id = $1 FOR UPDATE",
        )
        .bind(organization_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?
        .ok_or_else(|| {
            AuthFailure::not_found("organization_not_found", "organization does not exist")
        })?;
        if confirm_name != name {
            return Err(AuthFailure::bad_request(
                "confirmation_mismatch",
                "confirm_name must equal the organization name",
            ));
        }
        let workspace_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM workspaces WHERE organization_id = $1 AND deleted_at IS NULL",
        )
        .bind(organization_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        if workspace_count != 0 {
            return Ok(Err(DeletedOrganizationBlocked { workspace_count }));
        }
        // The organization row cascades members, groups, invitations, deleted
        // workspaces and its audit events; record the deletion platform-wide.
        sqlx::query(
            "INSERT INTO platform_audit_events(event_id, occurred_at, action, resource_kind, \
             resource_id, resource_name, payload) VALUES($1, $2, 'organization.deleted', \
             'organization', $3, $4, $5)",
        )
        .bind(format!("pae_{}", Uuid::new_v4().simple()))
        .bind(Utc::now().to_rfc3339())
        .bind(organization_id)
        .bind(&name)
        .bind(json!({ "actor_kind": actor.kind(), "actor_id": actor.id() }))
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        sqlx::query("DELETE FROM organizations WHERE organization_id = $1")
            .bind(organization_id)
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        Ok(Ok(()))
    }

    // ---- Workspace --------------------------------------------------------

    pub(crate) async fn effective_workspace_access(
        &self,
        workspace_id: &str,
        actor: &AdminActor,
    ) -> Result<EffectiveWorkspaceAccess, AuthFailure> {
        self.workspace_role_as(workspace_id, actor).await?;
        let mode: String = sqlx::query_scalar(
            "SELECT access_mode FROM workspaces WHERE workspace_id = $1 AND deleted_at IS NULL",
        )
        .bind(workspace_id)
        .fetch_one(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        let rows = sqlx::query(
            "SELECT u.id, u.preferred_name, u.email, om.role AS org_role, ug.role AS direct_role, \
             COALESCE((SELECT jsonb_agg(jsonb_build_array(g.name, wg.role) ORDER BY lower(g.name), g.group_id) \
               FROM workspace_group_grants wg JOIN organization_groups g ON g.group_id = wg.group_id \
               JOIN organization_group_members gm ON gm.group_id = wg.group_id \
               WHERE wg.workspace_id = w.workspace_id AND gm.user_id = u.id), '[]'::jsonb) AS groups \
             FROM workspaces w \
             JOIN organization_members om ON om.organization_id = w.organization_id \
             JOIN users u ON u.id = om.user_id \
             LEFT JOIN workspace_user_grants ug ON ug.workspace_id = w.workspace_id AND ug.user_id = u.id \
             WHERE w.workspace_id = $1 AND w.deleted_at IS NULL \
             ORDER BY lower(u.preferred_name), u.id",
        )
        .bind(workspace_id)
        .fetch_all(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        let mut members = Vec::new();
        for row in rows {
            let org_role: String = row.get("org_role");
            let direct: Option<String> = row.get("direct_role");
            let groups: Vec<(String, String)> =
                serde_json::from_value(row.get("groups")).unwrap_or_default();
            let mut via = Vec::new();
            let mut owner = false;
            if matches!(org_role.as_str(), "owner" | "admin") {
                owner = true;
                via.push(AccessVia {
                    kind: AccessViaKind::OrgRole,
                    detail: org_role.clone(),
                });
            } else if mode == "organization" {
                via.push(AccessVia {
                    kind: AccessViaKind::OrgRole,
                    detail: org_role.clone(),
                });
            }
            for (name, role) in groups {
                owner |= role == "owner";
                via.push(AccessVia {
                    kind: AccessViaKind::Group,
                    detail: format!("{name} ({role})"),
                });
            }
            if let Some(role) = direct {
                owner |= role == "owner";
                via.push(AccessVia {
                    kind: AccessViaKind::Direct,
                    detail: role,
                });
            }
            if via.is_empty() {
                continue;
            }
            members.push(EffectiveAccessMember {
                user: UserSummary {
                    id: row.get("id"),
                    name: row.get("preferred_name"),
                    email: row.get("email"),
                },
                role: if owner { "owner" } else { "member" }.into(),
                via,
            });
        }
        members.sort_by_key(|member| member.role != "owner");
        Ok(EffectiveWorkspaceAccess { members, mode })
    }

    /// With `revoke_machines`, revokes every machine credential and deletes
    /// the machine records in the deletion transaction; the caller then drops
    /// in-memory runtime state.
    pub(crate) fn validate_workspace_deletion(
        name: &str,
        request: &DeleteWorkspaceRequest,
    ) -> Result<(), AuthFailure> {
        match request.confirm_name.as_deref() {
            Some(confirm) if confirm != name => Err(AuthFailure::bad_request(
                "confirmation_mismatch",
                "confirm_name must equal the workspace name",
            )),
            None if request.revoke_machines => Err(AuthFailure::bad_request(
                "confirmation_required",
                "revoke_machines requires confirm_name",
            )),
            _ => Ok(()),
        }
    }

    pub(super) async fn revoke_workspace_machines(
        transaction: &mut Transaction<'_, Postgres>,
        workspace_id: &str,
        now: &str,
    ) -> Result<(), AuthFailure> {
        sqlx::query(
            "UPDATE machines SET revoked_at = $1 WHERE workspace_id = $2 AND revoked_at IS NULL",
        )
        .bind(now)
        .bind(workspace_id)
        .execute(&mut **transaction)
        .await
        .map_err(AuthFailure::database)?;
        for table in [
            "machine_names",
            "machine_services",
            "agent_names",
            "virtual_network_hosts",
        ] {
            sqlx::query(&format!("DELETE FROM {table} WHERE workspace_id = $1"))
                .bind(workspace_id)
                .execute(&mut **transaction)
                .await
                .map_err(AuthFailure::database)?;
        }
        sqlx::query(
            "UPDATE machine_enrollments SET used_at = $1 WHERE workspace_id = $2 AND used_at IS NULL",
        )
        .bind(now)
        .bind(workspace_id)
        .execute(&mut **transaction)
        .await
        .map_err(AuthFailure::database)?;
        Ok(())
    }

    // ---- Account (humans) -------------------------------------------------

    pub(crate) async fn list_sessions(
        &self,
        current: &CurrentSession,
    ) -> Result<Vec<AuthSessionInfo>, AuthFailure> {
        let rows = sqlx::query(
            "SELECT token, session_id, created_at, last_seen_at, device_name, client FROM sessions \
             WHERE user_id = $1 AND expires_at > $2 ORDER BY created_at DESC LIMIT 200",
        )
        .bind(&current.user_id)
        .bind(Utc::now().to_rfc3339())
        .fetch_all(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        Ok(rows
            .into_iter()
            .map(|row| {
                let created_at = parse_rfc3339(&row.get::<String, _>("created_at"));
                let token: String = row.get("token");
                let device = row
                    .get::<Option<String>, _>("device_name")
                    .or_else(|| row.get::<Option<String>, _>("client"))
                    .unwrap_or_else(|| "Browser".into());
                AuthSessionInfo {
                    id: row.get("session_id"),
                    device,
                    ip: None,
                    created_at,
                    last_seen_at: row
                        .get::<Option<String>, _>("last_seen_at")
                        .map(|value| parse_rfc3339(&value))
                        .unwrap_or(created_at),
                    current: bool::from(token.as_bytes().ct_eq(current.token.as_bytes())),
                }
            })
            .collect())
    }

    pub(crate) async fn revoke_session(
        &self,
        current: &CurrentSession,
        session_id: &str,
    ) -> Result<(), AuthFailure> {
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        let token: Option<String> = sqlx::query_scalar(
            "DELETE FROM sessions WHERE user_id = $1 AND session_id = $2 RETURNING token",
        )
        .bind(&current.user_id)
        .bind(session_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        if token.is_none() {
            return Err(AuthFailure::not_found(
                "session_not_found",
                "session does not exist",
            ));
        }
        transaction.commit().await.map_err(AuthFailure::database)
    }

    /// Ends every other session and rotates the current one's token (the
    /// caller sets the returned session's cookie), so a token copied before
    /// the change stops working.
    pub(crate) async fn change_password(
        &self,
        current: &CurrentSession,
        current_password: &str,
        new_password: &str,
    ) -> Result<CurrentSession, AuthFailure> {
        let new_password = validate_new_password(new_password)?;
        self.reserve_login_attempt("user:", &current.email.to_ascii_lowercase())
            .await?;
        let hash: String = sqlx::query_scalar("SELECT password_hash FROM users WHERE id = $1")
            .bind(&current.user_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(AuthFailure::database)?
            .ok_or_else(|| {
                AuthFailure::bad_request(
                    "password_unavailable",
                    "this session has no password account",
                )
            })?;
        if !verify_password(current_password, &hash) {
            return Err(AuthFailure::bad_request(
                "invalid_current_password",
                "current password is incorrect",
            ));
        }
        self.clear_login_attempts("user:", &current.email.to_ascii_lowercase())
            .await?;
        let new_hash = hash_password(&new_password)?;
        let now = Utc::now().to_rfc3339();
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        sqlx::query("UPDATE users SET password_hash = $1 WHERE id = $2")
            .bind(new_hash)
            .bind(&current.user_id)
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        sqlx::query(
            "UPDATE password_reset_tokens SET used_at = $1 WHERE user_id = $2 AND used_at IS NULL",
        )
        .bind(&now)
        .bind(&current.user_id)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let rotated =
            sqlx::query("UPDATE sessions SET token = $1 WHERE user_id = $2 AND token = $3")
                .bind(&token)
                .bind(&current.user_id)
                .bind(&current.token)
                .execute(&mut *transaction)
                .await
                .map_err(AuthFailure::database)?
                .rows_affected();
        if rotated != 1 {
            return Err(AuthFailure::unauthorized(
                "authentication_required",
                "the current session ended; sign in again",
            ));
        }
        sqlx::query("DELETE FROM sessions WHERE user_id = $1 AND token <> $2")
            .bind(&current.user_id)
            .bind(&token)
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        Ok(CurrentSession {
            token,
            ..current.clone()
        })
    }

    /// Coarse last-seen tracking: at most one write per session per window.
    pub(super) async fn touch_session(&self, token: &str) {
        let now = Utc::now();
        let stale = (now - Duration::seconds(SESSION_LAST_SEEN_RESOLUTION_SECONDS)).to_rfc3339();
        let _ = sqlx::query(
            "UPDATE sessions SET last_seen_at = $1 WHERE token = $2 \
             AND (last_seen_at IS NULL OR last_seen_at < $3)",
        )
        .bind(now.to_rfc3339())
        .bind(token)
        .bind(stale)
        .execute(&self.pool)
        .await;
    }
}

/// Lock and return every `(user_id, role)` of an organization so owner-count
/// invariants are checked against a stable set.
pub(super) async fn lock_member_roles(
    transaction: &mut Transaction<'_, Postgres>,
    organization_id: &str,
) -> Result<Vec<(String, String)>, AuthFailure> {
    let rows = sqlx::query(
        "SELECT user_id, role FROM organization_members WHERE organization_id = $1 \
         ORDER BY user_id FOR UPDATE",
    )
    .bind(organization_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(AuthFailure::database)?;
    Ok(rows
        .into_iter()
        .map(|row| (row.get("user_id"), row.get("role")))
        .collect())
}

pub(super) fn owner_count(roles: &[(String, String)]) -> usize {
    roles.iter().filter(|(_, role)| role == "owner").count()
}

pub(super) fn last_owner_error() -> AuthFailure {
    last_owner()
}

pub(super) async fn remove_member_rows(
    transaction: &mut Transaction<'_, Postgres>,
    organization_id: &str,
    user_id: &str,
) -> Result<(), AuthFailure> {
    sqlx::query(
        "DELETE FROM organization_group_members gm USING organization_groups g \
         WHERE gm.group_id = g.group_id AND g.organization_id = $1 AND gm.user_id = $2",
    )
    .bind(organization_id)
    .bind(user_id)
    .execute(&mut **transaction)
    .await
    .map_err(AuthFailure::database)?;
    sqlx::query(
        "DELETE FROM workspace_user_grants g USING workspaces w \
         WHERE g.workspace_id = w.workspace_id AND w.organization_id = $1 AND g.user_id = $2",
    )
    .bind(organization_id)
    .bind(user_id)
    .execute(&mut **transaction)
    .await
    .map_err(AuthFailure::database)?;
    sqlx::query("DELETE FROM organization_members WHERE organization_id = $1 AND user_id = $2")
        .bind(organization_id)
        .bind(user_id)
        .execute(&mut **transaction)
        .await
        .map_err(AuthFailure::database)?;
    Ok(())
}

/// Apply an organization invitation's groups and workspace grants to a member
/// who just joined. Grants whose group/workspace no longer exists are skipped.
pub(super) async fn apply_invitation_grants(
    transaction: &mut Transaction<'_, Postgres>,
    token: &str,
    organization_id: &str,
    user_id: &str,
    now: &str,
) -> Result<(), AuthFailure> {
    sqlx::query(
        "INSERT INTO organization_group_members(group_id, user_id, added_at, added_by) \
         SELECT g.group_id, $3, $4, 'invitation' FROM invitations i \
         JOIN organization_groups g ON g.organization_id = i.organization_id \
           AND i.group_ids ? g.group_id \
         WHERE i.token = $1 AND i.organization_id = $2 \
         ON CONFLICT(group_id, user_id) DO NOTHING",
    )
    .bind(token)
    .bind(organization_id)
    .bind(user_id)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(AuthFailure::database)?;
    sqlx::query(
        "INSERT INTO workspace_user_grants(workspace_id, user_id, role, created_at, created_by) \
         SELECT w.workspace_id, $3, 'member', $4, 'invitation' FROM invitations i \
         JOIN workspaces w ON w.organization_id = i.organization_id \
           AND w.deleted_at IS NULL AND i.workspace_ids ? w.workspace_id \
         WHERE i.token = $1 AND i.organization_id = $2 \
         ON CONFLICT(workspace_id, user_id) DO NOTHING",
    )
    .bind(token)
    .bind(organization_id)
    .bind(user_id)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(AuthFailure::database)?;
    Ok(())
}

/// SQL predicate (over alias `i`) for an invitation that can still be used.
pub(super) const INVITATION_USABLE: &str =
    "i.used_at IS NULL AND i.revoked_at IS NULL AND (i.expires_at IS NULL OR i.expires_at > now())";

fn dedupe_ids(values: &[String], field: &str) -> Result<Vec<String>, AuthFailure> {
    let mut ids = values
        .iter()
        .map(|value| value.trim().to_string())
        .collect::<Vec<_>>();
    if ids.iter().any(|id| id.is_empty() || id.len() > 128) {
        return Err(AuthFailure::bad_request(
            "invalid_invitation_grants",
            &format!("{field} entries must be non-empty IDs"),
        ));
    }
    ids.sort();
    ids.dedup();
    if ids.len() > MAX_INVITATION_GRANTS {
        return Err(AuthFailure::bad_request(
            "invalid_invitation_grants",
            &format!("{field} accepts at most 64 IDs"),
        ));
    }
    Ok(ids)
}

fn parse_rfc3339(value: &str) -> chrono::DateTime<Utc> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .unwrap_or_default()
}
