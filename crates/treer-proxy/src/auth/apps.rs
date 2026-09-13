use super::*;

impl AuthStore {
    pub async fn list_agent_launch_profiles(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<AgentLaunchProfile>, AuthFailure> {
        let rows = sqlx::query(
            "SELECT profile_id, workspace_id, name, description, cwd, command, args, \
             created_at, created_by, updated_at, updated_by FROM agent_launch_profiles \
             WHERE workspace_id = $1 ORDER BY lower(name), profile_id",
        )
        .bind(workspace_id)
        .fetch_all(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        rows.into_iter()
            .map(agent_launch_profile_from_row)
            .collect()
    }

    pub async fn resolve_agent_launch_profile(
        &self,
        workspace_id: &str,
        target: &str,
    ) -> Result<AgentLaunchProfile, AuthFailure> {
        let row = sqlx::query(
            "SELECT profile_id, workspace_id, name, description, cwd, command, args, \
             created_at, created_by, updated_at, updated_by FROM agent_launch_profiles \
             WHERE workspace_id = $1 AND profile_id = $2",
        )
        .bind(workspace_id)
        .bind(target)
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        let row = match row {
            Some(row) => row,
            None => sqlx::query(
                "SELECT profile_id, workspace_id, name, description, cwd, command, args, \
                 created_at, created_by, updated_at, updated_by FROM agent_launch_profiles \
                 WHERE workspace_id = $1 AND lower(name) = lower($2)",
            )
            .bind(workspace_id)
            .bind(target.trim())
            .fetch_optional(&self.pool)
            .await
            .map_err(AuthFailure::database)?
            .ok_or_else(|| {
                AuthFailure::not_found(
                    "launch_profile_not_found",
                    "agent launch profile does not exist",
                )
            })?,
        };
        agent_launch_profile_from_row(row)
    }

    pub async fn create_agent_launch_profile(
        &self,
        workspace_id: &str,
        actor: ProfileMutationActor<'_>,
        request: CreateAgentLaunchProfileRequest,
    ) -> Result<AgentLaunchProfile, AuthFailure> {
        let now = Utc::now();
        let profile = AgentLaunchProfile {
            profile_id: format!("alp_{}", Uuid::new_v4().simple()),
            workspace_id: workspace_id.to_string(),
            name: validate_resource_name(&request.name, "launch profile")?,
            description: validate_launch_profile_description(&request.description)?,
            cwd: validate_launch_profile_cwd(&request.cwd)?,
            command: validate_launch_profile_command(&request.command)?,
            args: validate_launch_profile_args(request.args)?,
            created_at: now,
            created_by: actor.label.to_string(),
            updated_at: now,
            updated_by: actor.label.to_string(),
        };
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        sqlx::query(
            "INSERT INTO agent_launch_profiles(\
             profile_id, workspace_id, name, description, cwd, command, args, created_at, \
             created_by, updated_at, updated_by) VALUES($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(&profile.profile_id)
        .bind(&profile.workspace_id)
        .bind(&profile.name)
        .bind(&profile.description)
        .bind(&profile.cwd)
        .bind(&profile.command)
        .bind(json!(&profile.args))
        .bind(profile.created_at.to_rfc3339())
        .bind(&profile.created_by)
        .bind(profile.updated_at.to_rfc3339())
        .bind(&profile.updated_by)
        .execute(&mut *transaction)
        .await
        .map_err(launch_profile_write_error)?;
        insert_launch_profile_audit(&mut transaction, &profile, actor, "launch_profile.created")
            .await?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        Ok(profile)
    }

    pub async fn update_agent_launch_profile(
        &self,
        workspace_id: &str,
        target: &str,
        actor: ProfileMutationActor<'_>,
        request: UpdateAgentLaunchProfileRequest,
    ) -> Result<AgentLaunchProfile, AuthFailure> {
        let current = self
            .resolve_agent_launch_profile(workspace_id, target)
            .await?;
        let profile = AgentLaunchProfile {
            name: request
                .name
                .as_deref()
                .map(|name| validate_resource_name(name, "launch profile"))
                .transpose()?
                .unwrap_or(current.name),
            description: request
                .description
                .as_deref()
                .map(validate_launch_profile_description)
                .transpose()?
                .unwrap_or(current.description),
            cwd: request
                .cwd
                .as_deref()
                .map(validate_launch_profile_cwd)
                .transpose()?
                .unwrap_or(current.cwd),
            command: request
                .command
                .as_deref()
                .map(validate_launch_profile_command)
                .transpose()?
                .unwrap_or(current.command),
            args: request
                .args
                .map(validate_launch_profile_args)
                .transpose()?
                .unwrap_or(current.args),
            updated_at: Utc::now(),
            updated_by: actor.label.to_string(),
            ..current
        };
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        let result = sqlx::query(
            "UPDATE agent_launch_profiles SET name = $1, description = $2, cwd = $3, \
             command = $4, args = $5, updated_at = $6, updated_by = $7 \
             WHERE workspace_id = $8 AND profile_id = $9",
        )
        .bind(&profile.name)
        .bind(&profile.description)
        .bind(&profile.cwd)
        .bind(&profile.command)
        .bind(json!(&profile.args))
        .bind(profile.updated_at.to_rfc3339())
        .bind(&profile.updated_by)
        .bind(workspace_id)
        .bind(&profile.profile_id)
        .execute(&mut *transaction)
        .await
        .map_err(launch_profile_write_error)?;
        if result.rows_affected() != 1 {
            return Err(AuthFailure::not_found(
                "launch_profile_not_found",
                "agent launch profile does not exist",
            ));
        }
        insert_launch_profile_audit(&mut transaction, &profile, actor, "launch_profile.updated")
            .await?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        Ok(profile)
    }

    pub async fn delete_agent_launch_profile(
        &self,
        workspace_id: &str,
        target: &str,
        actor: ProfileMutationActor<'_>,
    ) -> Result<AgentLaunchProfile, AuthFailure> {
        let profile = self
            .resolve_agent_launch_profile(workspace_id, target)
            .await?;
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        let result = sqlx::query(
            "DELETE FROM agent_launch_profiles WHERE workspace_id = $1 AND profile_id = $2",
        )
        .bind(workspace_id)
        .bind(&profile.profile_id)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        if result.rows_affected() != 1 {
            return Err(AuthFailure::not_found(
                "launch_profile_not_found",
                "agent launch profile does not exist",
            ));
        }
        insert_launch_profile_audit(&mut transaction, &profile, actor, "launch_profile.deleted")
            .await?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        Ok(profile)
    }

    pub async fn list_app_deployments(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<AppDeployment>, AuthFailure> {
        let rows = sqlx::query(
            "SELECT app_id, workspace_id, name, server_id, command, args, cwd, port, hostname, \
             service_id, desired_state, runtime_agent_id, restart_count, last_error, \
             created_at, created_by, updated_at, updated_by FROM app_deployments \
             WHERE workspace_id = $1 ORDER BY lower(name), app_id",
        )
        .bind(workspace_id)
        .fetch_all(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        rows.into_iter().map(app_deployment_from_row).collect()
    }

    pub async fn list_app_deployments_for_server(
        &self,
        workspace_id: &str,
        server_id: &str,
    ) -> Result<Vec<AppDeployment>, AuthFailure> {
        let rows = sqlx::query(
            "SELECT app_id, workspace_id, name, server_id, command, args, cwd, port, hostname, \
             service_id, desired_state, runtime_agent_id, restart_count, last_error, \
             created_at, created_by, updated_at, updated_by FROM app_deployments \
             WHERE workspace_id = $1 AND server_id = $2 ORDER BY app_id",
        )
        .bind(workspace_id)
        .bind(server_id)
        .fetch_all(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        rows.into_iter().map(app_deployment_from_row).collect()
    }

    pub async fn resolve_app_deployment(
        &self,
        workspace_id: &str,
        target: &str,
    ) -> Result<AppDeployment, AuthFailure> {
        let row = sqlx::query(
            "SELECT app_id, workspace_id, name, server_id, command, args, cwd, port, hostname, \
             service_id, desired_state, runtime_agent_id, restart_count, last_error, \
             created_at, created_by, updated_at, updated_by FROM app_deployments \
             WHERE workspace_id = $1 AND (app_id = $2 OR lower(name) = lower($2))",
        )
        .bind(workspace_id)
        .bind(target.trim())
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)?
        .ok_or_else(|| AuthFailure::not_found("app_not_found", "App does not exist"))?;
        app_deployment_from_row(row)
    }

    pub async fn resolve_app_deployment_by_runtime(
        &self,
        workspace_id: &str,
        runtime_agent_id: &str,
    ) -> Result<Option<AppDeployment>, AuthFailure> {
        let row = sqlx::query(
            "SELECT app_id, workspace_id, name, server_id, command, args, cwd, port, hostname, \
             service_id, desired_state, runtime_agent_id, restart_count, last_error, \
             created_at, created_by, updated_at, updated_by FROM app_deployments \
             WHERE workspace_id = $1 AND runtime_agent_id = $2",
        )
        .bind(workspace_id)
        .bind(runtime_agent_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        row.map(app_deployment_from_row).transpose()
    }

    pub async fn create_app_deployment(
        &self,
        workspace_id: &str,
        actor: &str,
        server_id: String,
        request: CreateAppDeploymentRequest,
    ) -> Result<AppDeployment, AuthFailure> {
        let update_guard = self.virtual_hosts_update.lock().await;
        let name = validate_resource_name(&request.name, "App")?;
        let command = validate_launch_profile_command(&request.command)?;
        let args = validate_launch_profile_args(request.args)?;
        let cwd = validate_launch_profile_cwd(&request.cwd)?;
        if request.port == 0 {
            return Err(AuthFailure::bad_request(
                "invalid_app",
                "App port must be between 1 and 65535",
            ));
        }
        let hostname = normalize_virtual_hostname(&request.hostname)?;
        let now = Utc::now();
        let service = MachineService {
            service_id: format!("svc_{}", Uuid::new_v4().simple()),
            workspace_id: workspace_id.to_string(),
            name: name.clone(),
            server_id: server_id.clone(),
            target_agent_id: None,
            target_host: "127.0.0.1".to_string(),
            target_port: request.port,
            protocol: MachineServiceProtocol::Http,
            created_at: now,
            created_by: actor.to_string(),
            updated_at: now,
            updated_by: actor.to_string(),
        };
        let deployment = AppDeployment {
            app_id: format!("app_{}", Uuid::new_v4().simple()),
            workspace_id: workspace_id.to_string(),
            name,
            server_id,
            command,
            args,
            cwd,
            port: request.port,
            hostname: hostname.clone(),
            service_id: service.service_id.clone(),
            public_url: None,
            access: None,
            desired_state: AppDesiredState::Running,
            runtime_agent_id: None,
            restart_count: 0,
            status: AppDeploymentStatus::Pending,
            pid: None,
            exit_code: None,
            last_error: None,
            created_at: now,
            created_by: actor.to_string(),
            updated_at: now,
            updated_by: actor.to_string(),
        };
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        sqlx::query(
            "INSERT INTO machine_services(service_id, workspace_id, name, server_id, target_agent_id, \
             target_host, target_port, protocol, created_at, created_by, updated_at, updated_by) \
             VALUES($1, $2, $3, $4, NULL, $5, $6, 'http', $7, $8, $9, $10)",
        )
        .bind(&service.service_id)
        .bind(&service.workspace_id)
        .bind(&service.name)
        .bind(&service.server_id)
        .bind(&service.target_host)
        .bind(i64::from(service.target_port))
        .bind(service.created_at.to_rfc3339())
        .bind(&service.created_by)
        .bind(service.updated_at.to_rfc3339())
        .bind(&service.updated_by)
        .execute(&mut *transaction)
        .await
        .map_err(app_deployment_write_error)?;
        sqlx::query(
            "INSERT INTO virtual_network_hosts(workspace_id, hostname, service_id, created_at, created_by) \
             VALUES($1, $2, $3, $4, $5)",
        )
        .bind(workspace_id)
        .bind(&hostname)
        .bind(&service.service_id)
        .bind(now.to_rfc3339())
        .bind(actor)
        .execute(&mut *transaction)
        .await
        .map_err(app_deployment_write_error)?;
        sqlx::query(
            "INSERT INTO app_deployments(app_id, workspace_id, name, server_id, command, args, cwd, \
             port, hostname, service_id, desired_state, runtime_agent_id, restart_count, last_error, \
             created_at, created_by, updated_at, updated_by) \
             VALUES($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'running', NULL, 0, NULL, $11, $12, $13, $14)",
        )
        .bind(&deployment.app_id)
        .bind(&deployment.workspace_id)
        .bind(&deployment.name)
        .bind(&deployment.server_id)
        .bind(&deployment.command)
        .bind(serde_json::to_value(&deployment.args).map_err(|error| {
            AuthFailure::internal("app_encode_error", error.to_string())
        })?)
        .bind(&deployment.cwd)
        .bind(i64::from(deployment.port))
        .bind(&deployment.hostname)
        .bind(&deployment.service_id)
        .bind(deployment.created_at.to_rfc3339())
        .bind(&deployment.created_by)
        .bind(deployment.updated_at.to_rfc3339())
        .bind(&deployment.updated_by)
        .execute(&mut *transaction)
        .await
        .map_err(app_deployment_write_error)?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        drop(update_guard);
        self.refresh_virtual_network_hosts()
            .await
            .map_err(|error| {
                AuthFailure::internal("virtual_host_refresh_failed", format!("{error:#}"))
            })?;
        Ok(deployment)
    }

    pub async fn claim_app_runtime(
        &self,
        workspace_id: &str,
        app_id: &str,
        expected_runtime_agent_id: Option<&str>,
        runtime_agent_id: &str,
        actor: &str,
    ) -> Result<Option<AppDeployment>, AuthFailure> {
        let row = sqlx::query(
            "UPDATE app_deployments SET runtime_agent_id = $1, \
             restart_count = restart_count + CASE WHEN runtime_agent_id IS NULL THEN 0 ELSE 1 END, \
             last_error = NULL, updated_at = $2, updated_by = $3 \
             WHERE workspace_id = $4 AND app_id = $5 AND desired_state = 'running' \
             AND runtime_agent_id IS NOT DISTINCT FROM $6 \
             RETURNING app_id, workspace_id, name, server_id, command, args, cwd, port, hostname, \
             service_id, desired_state, runtime_agent_id, restart_count, last_error, \
             created_at, created_by, updated_at, updated_by",
        )
        .bind(runtime_agent_id)
        .bind(Utc::now().to_rfc3339())
        .bind(actor)
        .bind(workspace_id)
        .bind(app_id)
        .bind(expected_runtime_agent_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        row.map(app_deployment_from_row).transpose()
    }

    pub async fn set_app_desired_state(
        &self,
        workspace_id: &str,
        target: &str,
        desired_state: AppDesiredState,
        actor: &str,
    ) -> Result<AppDeployment, AuthFailure> {
        let current = self.resolve_app_deployment(workspace_id, target).await?;
        let row = sqlx::query(
            "UPDATE app_deployments SET desired_state = $1, updated_at = $2, updated_by = $3 \
             WHERE workspace_id = $4 AND app_id = $5 RETURNING app_id, workspace_id, name, server_id, \
             command, args, cwd, port, hostname, service_id, desired_state, runtime_agent_id, \
             restart_count, last_error, created_at, created_by, updated_at, updated_by",
        )
        .bind(app_desired_state_str(desired_state))
        .bind(Utc::now().to_rfc3339())
        .bind(actor)
        .bind(workspace_id)
        .bind(&current.app_id)
        .fetch_one(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        app_deployment_from_row(row)
    }

    pub async fn set_app_last_error(
        &self,
        workspace_id: &str,
        app_id: &str,
        error: Option<&str>,
    ) -> Result<(), AuthFailure> {
        sqlx::query(
            "UPDATE app_deployments SET last_error = $1, updated_at = $2 \
             WHERE workspace_id = $3 AND app_id = $4",
        )
        .bind(error)
        .bind(Utc::now().to_rfc3339())
        .bind(workspace_id)
        .bind(app_id)
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        Ok(())
    }

    pub async fn delete_app_deployment(
        &self,
        workspace_id: &str,
        target: &str,
    ) -> Result<AppDeployment, AuthFailure> {
        let update_guard = self.virtual_hosts_update.lock().await;
        let _ingress_update = self.service_ingresses_update.lock().await;
        let deployment = self.resolve_app_deployment(workspace_id, target).await?;
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        sqlx::query("DELETE FROM app_deployments WHERE workspace_id = $1 AND app_id = $2")
            .bind(workspace_id)
            .bind(&deployment.app_id)
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        sqlx::query("DELETE FROM machine_services WHERE workspace_id = $1 AND service_id = $2")
            .bind(workspace_id)
            .bind(&deployment.service_id)
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        self.service_ingresses
            .write()
            .await
            .retain(|_, resolved| resolved.ingress.service_id != deployment.service_id);
        drop(update_guard);
        self.refresh_virtual_network_hosts()
            .await
            .map_err(|error| {
                AuthFailure::internal("virtual_host_refresh_failed", format!("{error:#}"))
            })?;
        Ok(deployment)
    }
}
