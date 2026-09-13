use super::*;

impl AuthStore {
    pub(crate) fn authentication_disabled(&self) -> bool {
        self.disabled
    }

    pub(super) async fn membership_role(
        &self,
        organization_id: &str,
        user_id: &str,
    ) -> Result<Option<String>, AuthFailure> {
        if self.disabled {
            let exists = sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM organizations WHERE organization_id = $1",
            )
            .bind(organization_id)
            .fetch_one(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
            return Ok((exists != 0).then(|| "owner".to_string()));
        }
        sqlx::query_scalar(
            "SELECT role FROM organization_members \
             WHERE organization_id = $1 AND user_id = $2",
        )
        .bind(organization_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)
    }

    pub(super) async fn require_manager(
        &self,
        organization_id: &str,
        user_id: &str,
    ) -> Result<String, AuthFailure> {
        let role = self
            .require_organization_member(organization_id, user_id)
            .await?;
        if matches!(role.as_str(), "owner" | "admin") {
            Ok(role)
        } else {
            Err(AuthFailure::forbidden(
                "organization_manager_required",
                "organization owner or administrator access required",
            ))
        }
    }

    pub async fn set_machine_name(
        &self,
        workspace_id: &str,
        server_id: &str,
        name: &str,
    ) -> Result<(), AuthFailure> {
        sqlx::query(
            "INSERT INTO machine_names(server_id, workspace_id, name, updated_at) \
             VALUES($1, $2, $3, $4) \
             ON CONFLICT(server_id) DO UPDATE SET \
             workspace_id = excluded.workspace_id, name = excluded.name, \
             updated_at = excluded.updated_at",
        )
        .bind(server_id)
        .bind(workspace_id)
        .bind(name)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        Ok(())
    }

    pub async fn set_agent_name(
        &self,
        workspace_id: &str,
        agent_id: &str,
        name: &str,
    ) -> Result<(), AuthFailure> {
        sqlx::query(
            "INSERT INTO agent_names(agent_id, workspace_id, name, updated_at) \
             VALUES($1, $2, $3, $4) \
             ON CONFLICT(agent_id) DO UPDATE SET \
             workspace_id = excluded.workspace_id, name = excluded.name, \
             updated_at = excluded.updated_at",
        )
        .bind(agent_id)
        .bind(workspace_id)
        .bind(name)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        Ok(())
    }

    pub async fn apply_server_name(&self, server: &mut ServerInfo) -> Result<(), AuthFailure> {
        if server.name.trim().is_empty() {
            server.name.clone_from(&server.hostname);
        }
        let name = sqlx::query_scalar::<_, String>(
            "SELECT name FROM machine_names WHERE server_id = $1 AND workspace_id = $2",
        )
        .bind(&server.server_id)
        .bind(&server.workspace_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        if let Some(name) = name {
            server.name = name;
        }
        Ok(())
    }

    pub async fn apply_agent_names(
        &self,
        snapshot: &mut AgentServerSnapshot,
    ) -> Result<Vec<String>, AuthFailure> {
        let rows = sqlx::query("SELECT agent_id, name FROM agent_names WHERE workspace_id = $1")
            .bind(&snapshot.server.workspace_id)
            .fetch_all(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
        let names: HashMap<String, String> = rows
            .into_iter()
            .map(|row| (row.get("agent_id"), row.get("name")))
            .collect();
        for agent in &mut snapshot.agents {
            if let Some(name) = names.get(&agent.agent_id) {
                agent.name.clone_from(name);
            }
        }
        let deleted = sqlx::query_scalar::<_, String>(
            "SELECT agent_id FROM deleted_agents WHERE workspace_id = $1",
        )
        .bind(&snapshot.server.workspace_id)
        .fetch_all(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        let deleted_set: std::collections::HashSet<_> =
            deleted.iter().map(String::as_str).collect();
        snapshot
            .agents
            .retain(|agent| !deleted_set.contains(agent.agent_id.as_str()));
        Ok(deleted)
    }

    pub async fn delete_agent(
        &self,
        workspace_id: &str,
        agent_id: &str,
    ) -> Result<(), AuthFailure> {
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        sqlx::query(
            "INSERT INTO deleted_agents(agent_id, workspace_id, deleted_at) \
             VALUES($1, $2, $3) \
             ON CONFLICT(agent_id) DO UPDATE SET \
             workspace_id = excluded.workspace_id, deleted_at = excluded.deleted_at",
        )
        .bind(agent_id)
        .bind(workspace_id)
        .bind(Utc::now().to_rfc3339())
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        sqlx::query("DELETE FROM agent_names WHERE agent_id = $1 AND workspace_id = $2")
            .bind(agent_id)
            .bind(workspace_id)
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        sqlx::query(
            "DELETE FROM machine_services WHERE workspace_id = $1 AND target_agent_id = $2",
        )
        .bind(workspace_id)
        .bind(agent_id)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        sqlx::query(
            "UPDATE agent_credentials SET revoked_at = $1 \
             WHERE agent_id = $2 AND workspace_id = $3 AND revoked_at IS NULL",
        )
        .bind(Utc::now().to_rfc3339())
        .bind(agent_id)
        .bind(workspace_id)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        self.agent_credentials.write().await.remove(agent_id);
        Ok(())
    }

    pub async fn delete_machine(
        &self,
        workspace_id: &str,
        server_id: &str,
        agent_ids: &[String],
    ) -> Result<(), AuthFailure> {
        let _update = self.virtual_hosts_update.lock().await;
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        if !self.disabled {
            let update = sqlx::query(
                "UPDATE machines SET revoked_at = $1 \
                 WHERE server_id = $2 AND workspace_id = $3 AND revoked_at IS NULL",
            )
            .bind(Utc::now().to_rfc3339())
            .bind(server_id)
            .bind(workspace_id)
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
            if update.rows_affected() != 1 {
                return Err(AuthFailure::conflict(
                    "machine_already_deleted",
                    "machine credential is already revoked",
                ));
            }
        }
        sqlx::query("DELETE FROM machine_names WHERE server_id = $1 AND workspace_id = $2")
            .bind(server_id)
            .bind(workspace_id)
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        sqlx::query("DELETE FROM machine_services WHERE workspace_id = $1 AND server_id = $2")
            .bind(workspace_id)
            .bind(server_id)
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        for agent_id in agent_ids {
            sqlx::query("DELETE FROM agent_names WHERE agent_id = $1 AND workspace_id = $2")
                .bind(agent_id)
                .bind(workspace_id)
                .execute(&mut *transaction)
                .await
                .map_err(AuthFailure::database)?;
            sqlx::query("DELETE FROM deleted_agents WHERE agent_id = $1 AND workspace_id = $2")
                .bind(agent_id)
                .bind(workspace_id)
                .execute(&mut *transaction)
                .await
                .map_err(AuthFailure::database)?;
            sqlx::query(
                "UPDATE agent_credentials SET revoked_at = $1 \
                 WHERE agent_id = $2 AND workspace_id = $3 AND revoked_at IS NULL",
            )
            .bind(Utc::now().to_rfc3339())
            .bind(agent_id)
            .bind(workspace_id)
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        }
        transaction.commit().await.map_err(AuthFailure::database)?;
        let mut credentials = self.agent_credentials.write().await;
        for agent_id in agent_ids {
            credentials.remove(agent_id);
        }
        drop(credentials);
        if let Some(hosts) = self.virtual_hosts.write().await.get_mut(workspace_id) {
            hosts.retain(|_, host| host.destination_server_id != server_id);
        }
        self.virtual_hosts_revision.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    pub async fn create_machine_enrollment(
        &self,
        workspace_id: &str,
        created_by: &str,
    ) -> Result<String, AuthFailure> {
        let enrollment_id = Uuid::new_v4().simple().to_string();
        let secret = random_secret();
        let enrollment = format_machine_enrollment_key(workspace_id, &enrollment_id, &secret)
            .map_err(|error| AuthFailure::internal("machine_enrollment_error", error.message))?;
        let identifier = enrollment
            .split_once('.')
            .map(|(identifier, _)| identifier)
            .ok_or_else(invalid_machine_enrollment)?;
        let secret_hash = hash_password(&secret)?;
        let now = Utc::now();
        let expires_at = now + Duration::minutes(MACHINE_ENROLLMENT_TTL_MINUTES);
        sqlx::query(
            "INSERT INTO machine_enrollments(\
             enrollment_id, workspace_id, secret_hash, created_at, expires_at, created_by) \
             VALUES($1, $2, $3, $4, $5, $6)",
        )
        .bind(identifier)
        .bind(workspace_id)
        .bind(secret_hash)
        .bind(now.to_rfc3339())
        .bind(expires_at.to_rfc3339())
        .bind(created_by)
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        Ok(enrollment)
    }

    #[cfg(test)]
    pub async fn claim_machine_enrollment(
        &self,
        token: &str,
    ) -> Result<MachineEnrollmentClaim, AuthFailure> {
        self.claim_machine_enrollment_for_installation(token, None, None, None)
            .await
    }

    pub async fn claim_machine_enrollment_for_installation(
        &self,
        token: &str,
        installation_id: Option<&str>,
        machine_name: Option<&str>,
        existing_server_id: Option<&str>,
    ) -> Result<MachineEnrollmentClaim, AuthFailure> {
        let installation_id = installation_id.map(validate_installation_id).transpose()?;
        let machine_name = machine_name
            .map(|name| validate_resource_name(name, "machine"))
            .transpose()?;
        let existing_server_id = existing_server_id
            .map(validate_machine_server_id)
            .transpose()?;
        if existing_server_id.is_some() && installation_id.is_none() {
            return Err(AuthFailure::bad_request(
                "invalid_machine_identity",
                "an installed machine ID requires an installation identity",
            ));
        }
        let enrollment =
            parse_machine_enrollment_key(token).map_err(|_| invalid_machine_enrollment())?;
        let now = Utc::now();
        let row = sqlx::query(
            "SELECT workspace_id, secret_hash, created_by \
             FROM machine_enrollments \
             WHERE enrollment_id = $1 AND used_at IS NULL AND expires_at > $2",
        )
        .bind(&enrollment.identifier)
        .bind(now.to_rfc3339())
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)?
        .ok_or_else(invalid_machine_enrollment)?;
        let secret_hash: String = row.get("secret_hash");
        if !verify_password(&enrollment.secret, &secret_hash) {
            return Err(invalid_machine_enrollment());
        }
        let workspace_id: String = row.get("workspace_id");
        if workspace_id != enrollment.workspace_id {
            return Err(invalid_machine_enrollment());
        }
        let created_by: String = row.get("created_by");
        let machine_secret = random_secret();
        let machine_secret_hash = hash_password(&machine_secret)?;
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        let workspace =
            sqlx::query("SELECT deleted_at FROM workspaces WHERE workspace_id = $1 FOR KEY SHARE")
                .bind(&workspace_id)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(AuthFailure::database)?;
        if workspace.is_some_and(|row| row.get::<Option<String>, _>("deleted_at").is_some()) {
            return Err(invalid_machine_enrollment());
        }
        let server_for_installation = if let Some(installation_id) = installation_id.as_deref() {
            sqlx::query_scalar::<_, String>(
                "SELECT server_id FROM machines WHERE workspace_id = $1 AND installation_id = $2",
            )
            .bind(&workspace_id)
            .bind(installation_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?
        } else {
            None
        };
        let server_id = match (
            server_for_installation.as_deref(),
            existing_server_id.as_deref(),
        ) {
            (Some(server_id), Some(expected)) if server_id != expected => {
                return Err(AuthFailure::conflict(
                    "machine_identity_conflict",
                    "this installation identity is already bound to another machine",
                ));
            }
            (Some(server_id), _) => server_id.to_string(),
            (None, Some(server_id)) => {
                let existing = sqlx::query(
                    "SELECT workspace_id, installation_id FROM machines WHERE server_id = $1",
                )
                .bind(server_id)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(AuthFailure::database)?;
                if let Some(existing) = existing {
                    let existing_workspace: String = existing.get("workspace_id");
                    let existing_installation: Option<String> = existing.get("installation_id");
                    if existing_workspace != workspace_id
                        || existing_installation
                            .as_deref()
                            .is_some_and(|value| Some(value) != installation_id.as_deref())
                    {
                        return Err(AuthFailure::conflict(
                            "machine_identity_conflict",
                            "the installed machine is bound to another workspace or installation identity",
                        ));
                    }
                }
                server_id.to_string()
            }
            (None, None) => format!("srv_{}", Uuid::new_v4().simple()),
        };
        let update = sqlx::query(
            "UPDATE machine_enrollments SET used_at = $1, server_id = $2 \
             WHERE enrollment_id = $3 AND used_at IS NULL AND expires_at > $4",
        )
        .bind(now.to_rfc3339())
        .bind(&server_id)
        .bind(&enrollment.identifier)
        .bind(now.to_rfc3339())
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        if update.rows_affected() != 1 {
            return Err(invalid_machine_enrollment());
        }
        let existing_machine =
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM machines WHERE server_id = $1")
                .bind(&server_id)
                .fetch_one(&mut *transaction)
                .await
                .map_err(AuthFailure::database)?
                != 0;
        if existing_machine {
            sqlx::query(
                "UPDATE machines SET installation_id = $1, secret_hash = $2, enrolled_by = $3, revoked_at = NULL \
                 WHERE server_id = $4 AND workspace_id = $5",
            )
            .bind(installation_id.as_deref())
            .bind(machine_secret_hash)
            .bind(&created_by)
            .bind(&server_id)
            .bind(&workspace_id)
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        } else {
            sqlx::query(
                "INSERT INTO machines(\
                 server_id, workspace_id, installation_id, secret_hash, created_at, enrolled_by) \
                 VALUES($1, $2, $3, $4, $5, $6)",
            )
            .bind(&server_id)
            .bind(&workspace_id)
            .bind(installation_id.as_deref())
            .bind(machine_secret_hash)
            .bind(now.to_rfc3339())
            .bind(&created_by)
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        }
        if let Some(machine_name) = machine_name {
            sqlx::query(
                "INSERT INTO machine_names(server_id, workspace_id, name, updated_at) \
                 VALUES($1, $2, $3, $4) ON CONFLICT(server_id) DO UPDATE SET \
                 workspace_id = excluded.workspace_id, name = excluded.name, updated_at = excluded.updated_at",
            )
            .bind(&server_id)
            .bind(&workspace_id)
            .bind(machine_name)
            .bind(now.to_rfc3339())
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        }
        transaction.commit().await.map_err(AuthFailure::database)?;
        Ok(MachineEnrollmentClaim {
            workspace_id,
            machine_token: format!("{server_id}.{machine_secret}"),
            server_id,
        })
    }

    pub async fn claim_machine_enrollment_from_headers(
        &self,
        headers: &HeaderMap,
        installation_id: Option<&str>,
        machine_name: Option<&str>,
        existing_server_id: Option<&str>,
    ) -> Result<MachineEnrollmentClaim, AuthFailure> {
        let token = bearer_token(headers).ok_or_else(invalid_machine_enrollment)?;
        self.claim_machine_enrollment_for_installation(
            token,
            installation_id,
            machine_name,
            existing_server_id,
        )
        .await
    }

    pub async fn bind_machine_identity(
        &self,
        workspace_id: &str,
        server_id: &str,
        installation_id: &str,
        machine_name: &str,
    ) -> Result<(), AuthFailure> {
        let installation_id = validate_installation_id(installation_id)?;
        let machine_name = validate_resource_name(machine_name, "machine")?;
        let now = Utc::now().to_rfc3339();
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        let update = sqlx::query(
            "UPDATE machines SET installation_id = $1 \
             WHERE workspace_id = $2 AND server_id = $3 AND revoked_at IS NULL \
             AND (installation_id IS NULL OR installation_id = $4)",
        )
        .bind(&installation_id)
        .bind(workspace_id)
        .bind(server_id)
        .bind(&installation_id)
        .execute(&mut *transaction)
        .await;
        match update {
            Ok(update) if update.rows_affected() == 1 => {}
            Ok(_) => {
                return Err(AuthFailure::conflict(
                    "machine_identity_conflict",
                    "this machine is already bound to another installation identity",
                ));
            }
            Err(error)
                if error
                    .as_database_error()
                    .is_some_and(|error| error.is_unique_violation()) =>
            {
                return Err(AuthFailure::conflict(
                    "machine_identity_conflict",
                    "this installation identity is already bound to another machine",
                ));
            }
            Err(error) => return Err(AuthFailure::database(error)),
        }
        sqlx::query(
            "INSERT INTO machine_names(server_id, workspace_id, name, updated_at) \
             VALUES($1, $2, $3, $4) ON CONFLICT(server_id) DO UPDATE SET \
             workspace_id = excluded.workspace_id, name = excluded.name, updated_at = excluded.updated_at",
        )
        .bind(server_id)
        .bind(workspace_id)
        .bind(machine_name)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)
    }

    pub async fn authenticate_machine(
        &self,
        headers: &HeaderMap,
    ) -> Result<MachineSession, AuthFailure> {
        if self.disabled {
            return Ok(MachineSession {
                server_id: None,
                workspace_id: None,
            });
        }
        let token = bearer_token(headers).ok_or_else(machine_auth_required)?;
        let (server_id, secret) = parse_credential(token, "srv_")?;
        let row = sqlx::query(
            "SELECT m.workspace_id, m.secret_hash FROM machines m \
             LEFT JOIN workspaces w ON w.workspace_id = m.workspace_id \
             WHERE m.server_id = $1 AND m.revoked_at IS NULL AND w.deleted_at IS NULL",
        )
        .bind(server_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)?
        .ok_or_else(machine_auth_required)?;
        let secret_hash: String = row.get("secret_hash");
        if !verify_password(secret, &secret_hash) {
            return Err(machine_auth_required());
        }
        Ok(MachineSession {
            server_id: Some(server_id.to_string()),
            workspace_id: Some(row.get("workspace_id")),
        })
    }

    pub async fn create_agent_credential(
        &self,
        workspace_id: &str,
        server_id: &str,
        agent_id: &str,
    ) -> Result<String, AuthFailure> {
        let credential = format!("wlc_{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let record = AgentCredentialRecord {
            workspace_id: workspace_id.to_string(),
            server_id: server_id.to_string(),
            secret_hash: fast_secret_hash(&credential),
            cached_at: Instant::now(),
        };
        sqlx::query(
            "INSERT INTO agent_credentials(agent_id, workspace_id, server_id, secret_hash, created_at) \
             VALUES($1, $2, $3, $4, $5) ON CONFLICT(agent_id) DO NOTHING",
        )
        .bind(agent_id)
        .bind(workspace_id)
        .bind(server_id)
        .bind(&record.secret_hash)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)
        .and_then(|result| {
            if result.rows_affected() == 1 {
                Ok(())
            } else {
                Err(AuthFailure::conflict(
                    "agent_credential_exists",
                    "Agent credential already exists",
                ))
            }
        })?;
        self.agent_credentials
            .write()
            .await
            .insert(agent_id.to_string(), record);
        Ok(credential)
    }

    pub async fn authenticate_agent(
        &self,
        machine: &MachineSession,
        headers: &HeaderMap,
    ) -> Result<Option<AgentSession>, AuthFailure> {
        let agent_id = optional_header(headers, AGENT_ID_HEADER)?;
        let credential = optional_header(headers, WORKLOAD_CREDENTIAL_HEADER)?;
        let (agent_id, credential) = match (agent_id, credential) {
            (None, None) => return Ok(None),
            (Some(agent_id), Some(credential)) => (agent_id, credential),
            _ => {
                return Err(AuthFailure::unauthorized(
                    "agent_authentication_required",
                    "Agent ID and workload credential are both required",
                ))
            }
        };
        let cached = self.agent_credentials.read().await.get(agent_id).cloned();
        let record = if let Some(record) =
            cached.filter(|record| record.cached_at.elapsed() < AGENT_CREDENTIAL_CACHE_TTL)
        {
            record
        } else {
            let row = sqlx::query(
                "SELECT c.workspace_id, c.server_id, c.secret_hash FROM agent_credentials c \
                 LEFT JOIN workspaces w ON w.workspace_id = c.workspace_id \
                 WHERE c.agent_id = $1 AND c.revoked_at IS NULL AND w.deleted_at IS NULL",
            )
            .bind(agent_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
            let Some(row) = row else {
                self.agent_credentials.write().await.remove(agent_id);
                return Err(agent_auth_required());
            };
            let record = AgentCredentialRecord {
                workspace_id: row.get("workspace_id"),
                server_id: row.get("server_id"),
                secret_hash: row.get("secret_hash"),
                cached_at: Instant::now(),
            };
            self.agent_credentials
                .write()
                .await
                .insert(agent_id.to_string(), record.clone());
            record
        };
        if !machine.allows_server(&record.workspace_id, &record.server_id)
            || !fast_secret_matches(credential, &record.secret_hash)
        {
            return Err(agent_auth_required());
        }
        Ok(Some(AgentSession {
            agent_id: agent_id.to_string(),
            server_id: record.server_id,
            workspace_id: record.workspace_id,
        }))
    }

    pub async fn active_agent_ids(
        &self,
        workspace_id: &str,
        server_id: &str,
        agent_ids: &[String],
    ) -> Result<Vec<String>, AuthFailure> {
        if agent_ids.is_empty() {
            return Ok(Vec::new());
        }
        if self.disabled {
            return Ok(agent_ids.to_vec());
        }
        sqlx::query_scalar::<_, String>(
            "SELECT c.agent_id FROM agent_credentials c \
             JOIN workspaces w ON w.workspace_id = c.workspace_id \
             WHERE c.workspace_id = $1 AND c.server_id = $2 \
             AND c.agent_id = ANY($3) AND c.revoked_at IS NULL AND w.deleted_at IS NULL \
             ORDER BY c.agent_id",
        )
        .bind(workspace_id)
        .bind(server_id)
        .bind(agent_ids)
        .fetch_all(&self.pool)
        .await
        .map_err(AuthFailure::database)
    }

    pub async fn machine_is_active(
        &self,
        workspace_id: &str,
        server_id: &str,
    ) -> Result<bool, AuthFailure> {
        if self.disabled {
            return Ok(true);
        }
        let count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM machines \
             WHERE workspace_id = $1 AND server_id = $2 AND revoked_at IS NULL",
        )
        .bind(workspace_id)
        .bind(server_id)
        .fetch_one(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        Ok(count == 1)
    }
}
