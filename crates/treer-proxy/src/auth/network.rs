use super::*;

impl AuthStore {
    pub async fn list_machine_services(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<MachineService>, AuthFailure> {
        let rows = sqlx::query(
            "SELECT service_id, workspace_id, name, server_id, target_agent_id, target_host, target_port, \
             protocol, created_at, created_by, updated_at, updated_by \
             FROM machine_services WHERE workspace_id = $1 \
             ORDER BY lower(name), service_id",
        )
        .bind(workspace_id)
        .fetch_all(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        rows.into_iter().map(machine_service_from_row).collect()
    }

    pub async fn resolve_machine_service(
        &self,
        workspace_id: &str,
        target: &str,
    ) -> Result<MachineService, AuthFailure> {
        let row = sqlx::query(
            "SELECT service_id, workspace_id, name, server_id, target_agent_id, target_host, target_port, \
             protocol, created_at, created_by, updated_at, updated_by \
             FROM machine_services WHERE workspace_id = $1 AND service_id = $2",
        )
        .bind(workspace_id)
        .bind(target)
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        let row = match row {
            Some(row) => row,
            None => sqlx::query(
                "SELECT service_id, workspace_id, name, server_id, target_agent_id, target_host, target_port, \
                 protocol, created_at, created_by, updated_at, updated_by \
                 FROM machine_services WHERE workspace_id = $1 AND lower(name) = lower($2)",
            )
            .bind(workspace_id)
            .bind(target.trim())
            .fetch_optional(&self.pool)
            .await
            .map_err(AuthFailure::database)?
            .ok_or_else(|| {
                AuthFailure::not_found("service_not_found", "machine service does not exist")
            })?,
        };
        machine_service_from_row(row)
    }

    pub async fn create_machine_service(
        &self,
        workspace_id: &str,
        actor: &str,
        request: CreateMachineServiceRequest,
    ) -> Result<MachineService, AuthFailure> {
        let name = validate_resource_name(&request.name, "service")?;
        let target_agent_id = request
            .target_agent_id
            .as_deref()
            .map(validate_service_target_agent_id)
            .transpose()?;
        let target_host = if target_agent_id.is_some() {
            validate_agent_service_target_host(&request.target_host)?
        } else {
            validate_service_target_host(&request.target_host)?
        };
        if request.target_port == 0 {
            return Err(AuthFailure::bad_request(
                "invalid_service",
                "target_port must be between 1 and 65535",
            ));
        }
        let now = Utc::now();
        let service = MachineService {
            service_id: format!("svc_{}", Uuid::new_v4().simple()),
            workspace_id: workspace_id.to_string(),
            name,
            server_id: request.server_id,
            target_agent_id,
            target_host,
            target_port: request.target_port,
            protocol: request.protocol,
            created_at: now,
            created_by: actor.to_string(),
            updated_at: now,
            updated_by: actor.to_string(),
        };
        sqlx::query(
            "INSERT INTO machine_services(\
             service_id, workspace_id, name, server_id, target_agent_id, target_host, target_port, protocol, \
             created_at, created_by, updated_at, updated_by) VALUES($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        )
        .bind(&service.service_id)
        .bind(&service.workspace_id)
        .bind(&service.name)
        .bind(&service.server_id)
        .bind(&service.target_agent_id)
        .bind(&service.target_host)
        .bind(i64::from(service.target_port))
        .bind(machine_service_protocol_str(service.protocol))
        .bind(service.created_at.to_rfc3339())
        .bind(&service.created_by)
        .bind(service.updated_at.to_rfc3339())
        .bind(&service.updated_by)
        .execute(&self.pool)
        .await
        .map_err(|error| {
            if error
                .as_database_error()
                .is_some_and(|error| error.is_unique_violation())
            {
                AuthFailure::conflict("service_exists", "service name already exists")
            } else {
                AuthFailure::database(error)
            }
        })?;
        Ok(service)
    }

    pub async fn update_machine_service(
        &self,
        workspace_id: &str,
        target: &str,
        actor: &str,
        request: UpdateMachineServiceRequest,
    ) -> Result<MachineService, AuthFailure> {
        let _update = self.virtual_hosts_update.lock().await;
        let current = self.resolve_machine_service(workspace_id, target).await?;
        if current.target_agent_id.is_some() {
            if request
                .server_id
                .as_ref()
                .is_some_and(|server_id| server_id != &current.server_id)
            {
                return Err(AuthFailure::bad_request(
                    "agent_service_scope_immutable",
                    "an Agent service cannot be moved to another machine",
                ));
            }
            if let Some(host) = request.target_host.as_deref() {
                validate_agent_service_target_host(host)?;
            }
        }
        let service = MachineService {
            name: request
                .name
                .as_deref()
                .map(|name| validate_resource_name(name, "service"))
                .transpose()?
                .unwrap_or(current.name),
            server_id: request.server_id.unwrap_or(current.server_id),
            target_host: request
                .target_host
                .as_deref()
                .map(validate_service_target_host)
                .transpose()?
                .unwrap_or(current.target_host),
            target_port: request.target_port.unwrap_or(current.target_port),
            protocol: request.protocol.unwrap_or(current.protocol),
            updated_at: Utc::now(),
            updated_by: actor.to_string(),
            ..current
        };
        if service.target_port == 0 {
            return Err(AuthFailure::bad_request(
                "invalid_service",
                "target_port must be between 1 and 65535",
            ));
        }
        sqlx::query(
            "UPDATE machine_services SET name = $1, server_id = $2, target_host = $3, \
             target_port = $4, protocol = $5, updated_at = $6, updated_by = $7 \
             WHERE workspace_id = $8 AND service_id = $9",
        )
        .bind(&service.name)
        .bind(&service.server_id)
        .bind(&service.target_host)
        .bind(i64::from(service.target_port))
        .bind(machine_service_protocol_str(service.protocol))
        .bind(service.updated_at.to_rfc3339())
        .bind(&service.updated_by)
        .bind(workspace_id)
        .bind(&service.service_id)
        .execute(&self.pool)
        .await
        .map_err(|error| {
            if error
                .as_database_error()
                .is_some_and(|error| error.is_unique_violation())
            {
                AuthFailure::conflict("service_exists", "service name already exists")
            } else {
                AuthFailure::database(error)
            }
        })?;
        if let Some(hosts) = self.virtual_hosts.write().await.get_mut(workspace_id) {
            for host in hosts
                .values_mut()
                .filter(|host| host.service_id == service.service_id)
            {
                host.service_protocol = service.protocol;
                host.destination_server_id.clone_from(&service.server_id);
                host.destination_agent_id
                    .clone_from(&service.target_agent_id);
                host.target_host.clone_from(&service.target_host);
                host.target_port = Some(service.target_port);
            }
        }
        self.virtual_hosts_revision.fetch_add(1, Ordering::SeqCst);
        Ok(service)
    }

    pub async fn delete_machine_service(
        &self,
        workspace_id: &str,
        target: &str,
    ) -> Result<MachineService, AuthFailure> {
        let _update = self.virtual_hosts_update.lock().await;
        let service = self.resolve_machine_service(workspace_id, target).await?;
        sqlx::query("DELETE FROM machine_services WHERE workspace_id = $1 AND service_id = $2")
            .bind(workspace_id)
            .bind(&service.service_id)
            .execute(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
        if let Some(hosts) = self.virtual_hosts.write().await.get_mut(workspace_id) {
            hosts.retain(|_, host| host.service_id != service.service_id);
        }
        self.virtual_hosts_revision.fetch_add(1, Ordering::SeqCst);
        Ok(service)
    }

    pub async fn list_virtual_network_hosts(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<VirtualNetworkHost>, AuthFailure> {
        let mut hosts = self
            .virtual_hosts
            .read()
            .await
            .get(workspace_id)
            .map(|hosts| hosts.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        hosts.sort_by(|left, right| left.hostname.cmp(&right.hostname));
        Ok(hosts)
    }

    pub async fn resolve_virtual_network_host(
        &self,
        workspace_id: &str,
        hostname: &str,
    ) -> Result<Option<VirtualNetworkHost>, AuthFailure> {
        let hostname = match normalize_virtual_hostname(hostname) {
            Ok(hostname) => hostname,
            Err(_) => return Ok(None),
        };
        Ok(self
            .virtual_hosts
            .read()
            .await
            .get(workspace_id)
            .and_then(|hosts| hosts.get(&hostname))
            .cloned())
    }

    pub async fn create_virtual_network_host(
        &self,
        workspace_id: &str,
        created_by: &str,
        request: CreateVirtualNetworkHostRequest,
    ) -> Result<VirtualNetworkHost, AuthFailure> {
        let _update = self.virtual_hosts_update.lock().await;
        let hostname = normalize_virtual_hostname(&request.hostname)?;
        let service = self
            .resolve_machine_service(workspace_id, &request.service_id)
            .await?;
        let record = VirtualNetworkHost {
            workspace_id: workspace_id.to_string(),
            hostname,
            service_id: service.service_id,
            service_protocol: service.protocol,
            destination_server_id: service.server_id,
            destination_agent_id: service.target_agent_id,
            target_host: service.target_host,
            target_port: Some(service.target_port),
            created_at: Utc::now(),
            created_by: created_by.to_string(),
        };
        let result = sqlx::query(
            "INSERT INTO virtual_network_hosts(\
             workspace_id, hostname, service_id, created_at, created_by) VALUES($1, $2, $3, $4, $5)",
        )
        .bind(&record.workspace_id)
        .bind(&record.hostname)
        .bind(&record.service_id)
        .bind(record.created_at.to_rfc3339())
        .bind(&record.created_by)
        .execute(&self.pool)
        .await;
        match result {
            Ok(_) => {
                self.virtual_hosts
                    .write()
                    .await
                    .entry(workspace_id.to_string())
                    .or_default()
                    .insert(record.hostname.clone(), record.clone());
                self.virtual_hosts_revision.fetch_add(1, Ordering::SeqCst);
                Ok(record)
            }
            Err(error)
                if error
                    .as_database_error()
                    .is_some_and(|error| error.is_unique_violation()) =>
            {
                Err(AuthFailure::conflict(
                    "virtual_host_exists",
                    "virtual hostname already exists in this workspace",
                ))
            }
            Err(error) => Err(AuthFailure::database(error)),
        }
    }

    pub async fn delete_virtual_network_host(
        &self,
        workspace_id: &str,
        hostname: &str,
    ) -> Result<(), AuthFailure> {
        let _update = self.virtual_hosts_update.lock().await;
        let hostname = normalize_virtual_hostname(hostname)?;
        let result = sqlx::query(
            "DELETE FROM virtual_network_hosts WHERE workspace_id = $1 AND hostname = $2",
        )
        .bind(workspace_id)
        .bind(&hostname)
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        if result.rows_affected() == 0 {
            return Err(AuthFailure::not_found(
                "virtual_host_not_found",
                "virtual host does not exist",
            ));
        }
        if let Some(hosts) = self.virtual_hosts.write().await.get_mut(workspace_id) {
            hosts.remove(&hostname);
        }
        self.virtual_hosts_revision.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    pub async fn refresh_virtual_network_hosts(&self) -> anyhow::Result<()> {
        let _update = self.virtual_hosts_update.lock().await;
        let rows = sqlx::query(
            "SELECT v.workspace_id, v.hostname, v.service_id, s.protocol AS service_protocol, \
             s.server_id AS destination_server_id, s.target_agent_id AS destination_agent_id, \
             s.target_host, s.target_port, v.created_at, v.created_by \
             FROM virtual_network_hosts v \
             JOIN machine_services s ON s.service_id = v.service_id \
             JOIN workspaces w ON w.workspace_id = v.workspace_id \
             WHERE w.deleted_at IS NULL",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut refreshed = HashMap::<String, HashMap<String, VirtualNetworkHost>>::new();
        for row in rows {
            let host = virtual_network_host_from_row(row)
                .map_err(|error| anyhow::anyhow!(error.into_parts().1.message))?;
            refreshed
                .entry(host.workspace_id.clone())
                .or_default()
                .insert(host.hostname.clone(), host);
        }
        *self.virtual_hosts.write().await = refreshed;
        self.virtual_hosts_revision.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    pub async fn virtual_network_hosts_snapshot(
        &self,
        workspace_id: &str,
    ) -> Result<treer_protocol::VirtualNetworkHostsSnapshot, AuthFailure> {
        let _update = self.virtual_hosts_update.lock().await;
        let mut hosts = self
            .virtual_hosts
            .read()
            .await
            .get(workspace_id)
            .map(|hosts| hosts.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        hosts.sort_by(|left, right| left.hostname.cmp(&right.hostname));
        Ok(treer_protocol::VirtualNetworkHostsSnapshot {
            workspace_id: workspace_id.to_string(),
            revision: self.virtual_hosts_revision.load(Ordering::SeqCst),
            hosts,
        })
    }

    pub async fn list_service_ingresses(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<ServiceIngress>, AuthFailure> {
        let mut ingresses = self
            .service_ingresses
            .read()
            .await
            .values()
            .filter(|resolved| resolved.ingress.workspace_id == workspace_id)
            .map(|resolved| resolved.ingress.clone())
            .collect::<Vec<_>>();
        ingresses.sort_by(|left, right| left.hostname.cmp(&right.hostname));
        Ok(ingresses)
    }

    pub async fn resolve_service_ingress(
        &self,
        workspace_id: &str,
        target: &str,
    ) -> Result<ResolvedServiceIngress, AuthFailure> {
        self.service_ingresses
            .read()
            .await
            .values()
            .find(|resolved| {
                resolved.ingress.workspace_id == workspace_id
                    && (resolved.ingress.ingress_id == target
                        || resolved.ingress.hostname.eq_ignore_ascii_case(target))
            })
            .cloned()
            .ok_or_else(|| {
                AuthFailure::not_found("ingress_not_found", "service ingress does not exist")
            })
    }

    pub async fn resolve_service_ingress_hostname(
        &self,
        hostname: &str,
    ) -> Result<Option<ResolvedServiceIngress>, AuthFailure> {
        if let Some(resolved) = self
            .service_ingresses
            .read()
            .await
            .get(&hostname.to_ascii_lowercase())
            .cloned()
        {
            return Ok(Some(resolved));
        }
        let row = sqlx::query(
            "SELECT i.ingress_id, i.workspace_id, i.service_id, i.hostname, i.access, i.enabled, \
             i.created_at, i.created_by, i.updated_at, i.updated_by, \
             s.name AS service_name, s.server_id, s.target_agent_id, s.target_host, s.target_port, \
             s.protocol AS service_protocol, s.created_at AS service_created_at, \
             s.created_by AS service_created_by, s.updated_at AS service_updated_at, \
             s.updated_by AS service_updated_by \
             FROM service_ingresses i JOIN machine_services s ON s.service_id = i.service_id \
             JOIN workspaces w ON w.workspace_id = i.workspace_id \
             WHERE lower(i.hostname) = lower($1) AND w.deleted_at IS NULL",
        )
        .bind(hostname)
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        let Some(row) = row else { return Ok(None) };
        let resolved = resolved_service_ingress_from_row(row)?;
        self.service_ingresses.write().await.insert(
            resolved.ingress.hostname.to_ascii_lowercase(),
            resolved.clone(),
        );
        Ok(Some(resolved))
    }

    pub async fn create_service_ingress(
        &self,
        workspace_id: &str,
        actor: &str,
        base_domain: &str,
        request: CreateServiceIngressRequest,
    ) -> Result<ServiceIngress, AuthFailure> {
        let _update = self.service_ingresses_update.lock().await;
        let service = self
            .resolve_machine_service(workspace_id, &request.service_id)
            .await?;
        if service.protocol != MachineServiceProtocol::Http {
            return Err(AuthFailure::bad_request(
                "service_protocol_mismatch",
                "public ingress requires an HTTP service",
            ));
        }
        let slug = normalize_ingress_slug(request.slug.as_deref().unwrap_or(&service.name))?;
        let suffix = Uuid::new_v4().simple().to_string();
        let hostname = format!("{slug}-{}.{}", &suffix[..8], base_domain);
        if hostname.len() > 253 {
            return Err(AuthFailure::bad_request(
                "invalid_ingress_slug",
                "generated ingress hostname is too long",
            ));
        }
        let now = Utc::now();
        let ingress = ServiceIngress {
            ingress_id: format!("ing_{}", Uuid::new_v4().simple()),
            workspace_id: workspace_id.to_string(),
            service_id: service.service_id.clone(),
            hostname: hostname.clone(),
            access: request.access,
            enabled: true,
            created_at: now,
            created_by: actor.to_string(),
            updated_at: now,
            updated_by: actor.to_string(),
        };
        sqlx::query(
            "INSERT INTO service_ingresses(\
             ingress_id, workspace_id, service_id, hostname, access, enabled, created_at, \
             created_by, updated_at, updated_by) VALUES($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(&ingress.ingress_id)
        .bind(&ingress.workspace_id)
        .bind(&ingress.service_id)
        .bind(&ingress.hostname)
        .bind(service_ingress_access_str(ingress.access))
        .bind(ingress.enabled)
        .bind(ingress.created_at.to_rfc3339())
        .bind(&ingress.created_by)
        .bind(ingress.updated_at.to_rfc3339())
        .bind(&ingress.updated_by)
        .execute(&self.pool)
        .await
        .map_err(|error| {
            if error
                .as_database_error()
                .is_some_and(|error| error.is_unique_violation())
            {
                AuthFailure::conflict("ingress_exists", "service ingress hostname already exists")
            } else {
                AuthFailure::database(error)
            }
        })?;
        self.service_ingresses.write().await.insert(
            hostname.to_ascii_lowercase(),
            ResolvedServiceIngress {
                ingress: ingress.clone(),
                service,
            },
        );
        Ok(ingress)
    }

    pub async fn ensure_app_ingress(
        &self,
        app: &AppDeployment,
        actor: &str,
        base_domain: &str,
        requested_access: Option<ServiceIngressAccess>,
    ) -> Result<ServiceIngress, AuthFailure> {
        let service = self
            .resolve_machine_service(&app.workspace_id, &app.service_id)
            .await?;
        let _update = self.service_ingresses_update.lock().await;
        let hostname = managed_app_ingress_hostname(&app.name, &app.app_id, base_domain)?;
        if let Some(existing) = self.service_ingresses.read().await.get(&hostname) {
            return if existing.ingress.service_id == app.service_id
                && (requested_access.is_none() || requested_access == Some(existing.ingress.access))
            {
                Ok(existing.ingress.clone())
            } else {
                Err(AuthFailure::conflict(
                    "ingress_exists",
                    "generated App ingress hostname already exists",
                ))
            };
        }
        let now = Utc::now();
        let access = requested_access.unwrap_or(ServiceIngressAccess::Workspace);
        let ingress = ServiceIngress {
            ingress_id: format!("ing_{}", Uuid::new_v4().simple()),
            workspace_id: app.workspace_id.clone(),
            service_id: app.service_id.clone(),
            hostname: hostname.clone(),
            access,
            enabled: true,
            created_at: now,
            created_by: actor.to_string(),
            updated_at: now,
            updated_by: actor.to_string(),
        };
        let inserted = sqlx::query(
            "INSERT INTO service_ingresses(\
             ingress_id, workspace_id, service_id, hostname, access, enabled, created_at, \
             created_by, updated_at, updated_by) VALUES($1, $2, $3, $4, $5, TRUE, $6, $7, $8, $9) \
             ON CONFLICT DO NOTHING",
        )
        .bind(&ingress.ingress_id)
        .bind(&ingress.workspace_id)
        .bind(&ingress.service_id)
        .bind(&ingress.hostname)
        .bind(service_ingress_access_str(ingress.access))
        .bind(ingress.created_at.to_rfc3339())
        .bind(&ingress.created_by)
        .bind(ingress.updated_at.to_rfc3339())
        .bind(&ingress.updated_by)
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        if inserted.rows_affected() == 0 {
            return self
                .resolve_service_ingress_hostname(&hostname)
                .await?
                .filter(|resolved| {
                    resolved.ingress.service_id == app.service_id
                        && (requested_access.is_none()
                            || requested_access == Some(resolved.ingress.access))
                })
                .map(|resolved| resolved.ingress)
                .ok_or_else(|| {
                    AuthFailure::conflict(
                        "ingress_exists",
                        "generated App ingress hostname already exists",
                    )
                });
        }
        self.service_ingresses.write().await.insert(
            hostname,
            ResolvedServiceIngress {
                ingress: ingress.clone(),
                service,
            },
        );
        Ok(ingress)
    }

    pub async fn ensure_managed_app_ingresses(
        &self,
        actor: &str,
        base_domain: &str,
    ) -> Result<usize, AuthFailure> {
        let rows = sqlx::query(
            "SELECT a.app_id, a.workspace_id, a.name, a.server_id, a.command, a.args, a.cwd, \
             a.port, a.hostname, a.service_id, a.desired_state, a.runtime_agent_id, \
             a.restart_count, a.last_error, a.created_at, a.created_by, a.updated_at, a.updated_by \
             FROM app_deployments a JOIN workspaces w ON w.workspace_id = a.workspace_id \
             WHERE w.deleted_at IS NULL ORDER BY a.app_id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        let apps = rows
            .into_iter()
            .map(app_deployment_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        let mut created = 0;
        for app in apps {
            let hostname = managed_app_ingress_hostname(&app.name, &app.app_id, base_domain)?;
            let existed = self.service_ingresses.read().await.contains_key(&hostname);
            self.ensure_app_ingress(&app, actor, base_domain, None)
                .await?;
            created += usize::from(!existed);
        }
        Ok(created)
    }

    pub async fn set_app_ingress_access(
        &self,
        app: &AppDeployment,
        actor: &str,
        base_domain: &str,
        access: ServiceIngressAccess,
    ) -> Result<ServiceIngress, AuthFailure> {
        let hostname = managed_app_ingress_hostname(&app.name, &app.app_id, base_domain)?;
        let Some(existing) = self.resolve_service_ingress_hostname(&hostname).await? else {
            return self
                .ensure_app_ingress(app, actor, base_domain, Some(access))
                .await;
        };
        if existing.ingress.workspace_id != app.workspace_id
            || existing.ingress.service_id != app.service_id
        {
            return Err(AuthFailure::conflict(
                "ingress_exists",
                "generated App ingress hostname already exists",
            ));
        }
        self.update_service_ingress(
            &app.workspace_id,
            &existing.ingress.ingress_id,
            actor,
            UpdateServiceIngressRequest {
                access: Some(access),
                enabled: Some(true),
            },
        )
        .await
    }

    pub async fn update_service_ingress(
        &self,
        workspace_id: &str,
        target: &str,
        actor: &str,
        request: UpdateServiceIngressRequest,
    ) -> Result<ServiceIngress, AuthFailure> {
        let _update = self.service_ingresses_update.lock().await;
        let current = self.resolve_service_ingress(workspace_id, target).await?;
        let mut ingress = current.ingress;
        ingress.access = request.access.unwrap_or(ingress.access);
        ingress.enabled = request.enabled.unwrap_or(ingress.enabled);
        ingress.updated_at = Utc::now();
        ingress.updated_by = actor.to_string();
        sqlx::query(
            "UPDATE service_ingresses SET access = $1, enabled = $2, updated_at = $3, \
             updated_by = $4 WHERE workspace_id = $5 AND ingress_id = $6",
        )
        .bind(service_ingress_access_str(ingress.access))
        .bind(ingress.enabled)
        .bind(ingress.updated_at.to_rfc3339())
        .bind(&ingress.updated_by)
        .bind(workspace_id)
        .bind(&ingress.ingress_id)
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        self.service_ingresses.write().await.insert(
            ingress.hostname.to_ascii_lowercase(),
            ResolvedServiceIngress {
                ingress: ingress.clone(),
                service: current.service,
            },
        );
        Ok(ingress)
    }

    pub async fn delete_service_ingress(
        &self,
        workspace_id: &str,
        target: &str,
    ) -> Result<ServiceIngress, AuthFailure> {
        let _update = self.service_ingresses_update.lock().await;
        let resolved = self.resolve_service_ingress(workspace_id, target).await?;
        sqlx::query("DELETE FROM service_ingresses WHERE workspace_id = $1 AND ingress_id = $2")
            .bind(workspace_id)
            .bind(&resolved.ingress.ingress_id)
            .execute(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
        self.service_ingresses
            .write()
            .await
            .remove(&resolved.ingress.hostname.to_ascii_lowercase());
        Ok(resolved.ingress)
    }

    pub async fn refresh_service_ingresses(&self) -> anyhow::Result<()> {
        let _update = self.service_ingresses_update.lock().await;
        let rows = sqlx::query(
            "SELECT i.ingress_id, i.workspace_id, i.service_id, i.hostname, i.access, i.enabled, \
             i.created_at, i.created_by, i.updated_at, i.updated_by, \
             s.name AS service_name, s.server_id, s.target_agent_id, s.target_host, s.target_port, \
             s.protocol AS service_protocol, s.created_at AS service_created_at, \
             s.created_by AS service_created_by, s.updated_at AS service_updated_at, \
             s.updated_by AS service_updated_by \
             FROM service_ingresses i JOIN machine_services s ON s.service_id = i.service_id \
             JOIN workspaces w ON w.workspace_id = i.workspace_id \
             WHERE w.deleted_at IS NULL",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut refreshed = HashMap::new();
        for row in rows {
            let resolved = resolved_service_ingress_from_row(row)
                .map_err(|error| anyhow::anyhow!(error.into_parts().1.message))?;
            refreshed.insert(resolved.ingress.hostname.to_ascii_lowercase(), resolved);
        }
        *self.service_ingresses.write().await = refreshed;
        Ok(())
    }

    pub async fn create_ingress_auth_code(
        &self,
        ingress: &ServiceIngress,
        user_id: &str,
        return_path: &str,
    ) -> Result<String, AuthFailure> {
        self.require_workspace_member(&ingress.workspace_id, user_id)
            .await?;
        let return_path = validate_ingress_return_path(return_path)?;
        let code = format!("iac_{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let now = Utc::now();
        sqlx::query("DELETE FROM ingress_auth_codes WHERE expires_at <= $1 OR used_at IS NOT NULL")
            .bind(now.to_rfc3339())
            .execute(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
        sqlx::query(
            "INSERT INTO ingress_auth_codes(code, ingress_id, user_id, return_path, expires_at) \
             VALUES($1, $2, $3, $4, $5)",
        )
        .bind(&code)
        .bind(&ingress.ingress_id)
        .bind(user_id)
        .bind(return_path)
        .bind((now + Duration::minutes(INGRESS_AUTH_CODE_TTL_MINUTES)).to_rfc3339())
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        Ok(code)
    }

    pub async fn consume_ingress_auth_code(
        &self,
        hostname: &str,
        code: &str,
    ) -> Result<ConsumedIngressAuthorization, AuthFailure> {
        let now = Utc::now();
        let row = sqlx::query(
            "SELECT c.ingress_id, c.user_id, c.return_path, i.workspace_id \
             FROM ingress_auth_codes c JOIN service_ingresses i ON i.ingress_id = c.ingress_id \
             WHERE c.code = $1 AND lower(i.hostname) = lower($2) AND i.enabled = TRUE \
             AND c.used_at IS NULL AND c.expires_at > $3",
        )
        .bind(code)
        .bind(hostname)
        .bind(now.to_rfc3339())
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)?
        .ok_or_else(invalid_ingress_authorization)?;
        let workspace_id: String = row.get("workspace_id");
        let user_id: String = row.get("user_id");
        self.require_workspace_member(&workspace_id, &user_id)
            .await?;
        let result = sqlx::query(
            "UPDATE ingress_auth_codes SET used_at = $1 WHERE code = $2 AND used_at IS NULL",
        )
        .bind(now.to_rfc3339())
        .bind(code)
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        if result.rows_affected() != 1 {
            return Err(invalid_ingress_authorization());
        }
        let session_token = format!("ias_{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        sqlx::query("DELETE FROM ingress_sessions WHERE expires_at <= $1")
            .bind(now.to_rfc3339())
            .execute(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
        sqlx::query(
            "INSERT INTO ingress_sessions(token, ingress_id, user_id, created_at, expires_at) \
             VALUES($1, $2, $3, $4, $5)",
        )
        .bind(&session_token)
        .bind(row.get::<String, _>("ingress_id"))
        .bind(&user_id)
        .bind(now.to_rfc3339())
        .bind((now + Duration::hours(INGRESS_SESSION_TTL_HOURS)).to_rfc3339())
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        Ok(ConsumedIngressAuthorization {
            session_token,
            return_path: row.get("return_path"),
        })
    }

    pub async fn authenticate_ingress_session(
        &self,
        hostname: &str,
        token: &str,
    ) -> Result<Option<String>, AuthFailure> {
        let row = sqlx::query(
            "SELECT s.user_id, i.workspace_id FROM ingress_sessions s \
             JOIN service_ingresses i ON i.ingress_id = s.ingress_id \
             WHERE s.token = $1 AND lower(i.hostname) = lower($2) AND i.enabled = TRUE \
             AND s.expires_at > $3",
        )
        .bind(token)
        .bind(hostname)
        .bind(Utc::now().to_rfc3339())
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        let Some(row) = row else { return Ok(None) };
        let user_id: String = row.get("user_id");
        let workspace_id: String = row.get("workspace_id");
        if self
            .require_workspace_member(&workspace_id, &user_id)
            .await
            .is_err()
        {
            return Ok(None);
        }
        Ok(Some(user_id))
    }

    pub async fn create_app_oauth_code(
        &self,
        grant: &AppOAuthGrant,
        redirect_uri: &str,
        code_challenge: &str,
    ) -> Result<String, AuthFailure> {
        if !valid_pkce_challenge(code_challenge) {
            return Err(AuthFailure::bad_request(
                "invalid_code_challenge",
                "app OAuth requires a valid S256 PKCE code challenge",
            ));
        }
        if !matches!(grant.role.as_str(), "owner" | "admin" | "member") {
            return Err(AuthFailure::bad_request(
                "invalid_app_identity",
                "app OAuth role is invalid",
            ));
        }
        let code = format!("aoc_{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let now = Utc::now();
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        sqlx::query("DELETE FROM app_oauth_codes WHERE expires_at <= $1 OR used_at IS NOT NULL")
            .bind(now.to_rfc3339())
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        sqlx::query(
            "INSERT INTO app_oauth_codes(\
             code_hash, workspace_id, service_id, user_id, preferred_name, role, redirect_uri, \
             code_challenge, created_at, expires_at) \
             VALUES($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(fast_secret_hash(&code))
        .bind(&grant.workspace_id)
        .bind(&grant.service_id)
        .bind(&grant.user_id)
        .bind(&grant.preferred_name)
        .bind(&grant.role)
        .bind(redirect_uri)
        .bind(code_challenge)
        .bind(now.to_rfc3339())
        .bind((now + Duration::minutes(APP_OAUTH_CODE_TTL_MINUTES)).to_rfc3339())
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        Ok(code)
    }

    pub async fn consume_app_oauth_code(
        &self,
        code: &str,
        service_id: &str,
        redirect_uri: &str,
        code_verifier: &str,
    ) -> Result<AppOAuthGrant, AuthFailure> {
        if !valid_pkce_verifier(code_verifier) {
            return Err(invalid_app_oauth_code());
        }
        let now = Utc::now();
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        let row = sqlx::query(
            "SELECT workspace_id, service_id, user_id, preferred_name, role, code_challenge \
             FROM app_oauth_codes WHERE code_hash = $1 AND service_id = $2 \
             AND redirect_uri = $3 AND used_at IS NULL AND expires_at > $4 FOR UPDATE",
        )
        .bind(fast_secret_hash(code))
        .bind(service_id)
        .bind(redirect_uri)
        .bind(now.to_rfc3339())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?
        .ok_or_else(invalid_app_oauth_code)?;
        let expected_challenge: String = row.get("code_challenge");
        let actual_challenge = pkce_challenge(code_verifier);
        if actual_challenge.len() != expected_challenge.len()
            || actual_challenge
                .as_bytes()
                .ct_eq(expected_challenge.as_bytes())
                .unwrap_u8()
                != 1
        {
            return Err(invalid_app_oauth_code());
        }
        let result = sqlx::query(
            "UPDATE app_oauth_codes SET used_at = $1 \
             WHERE code_hash = $2 AND used_at IS NULL",
        )
        .bind(now.to_rfc3339())
        .bind(fast_secret_hash(code))
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        if result.rows_affected() != 1 {
            return Err(invalid_app_oauth_code());
        }
        let grant = AppOAuthGrant {
            workspace_id: row.get("workspace_id"),
            service_id: row.get("service_id"),
            user_id: row.get("user_id"),
            preferred_name: row.get("preferred_name"),
            role: row.get("role"),
        };
        transaction.commit().await.map_err(AuthFailure::database)?;
        self.require_workspace_member(&grant.workspace_id, &grant.user_id)
            .await?;
        Ok(grant)
    }
}
