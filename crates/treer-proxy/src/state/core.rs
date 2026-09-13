use super::*;

impl AppState {
    pub fn new() -> Self {
        Self::with_event_bus(EventBus::in_process())
    }

    pub fn with_event_bus(event_bus: EventBus) -> Self {
        Self::with_backplanes(
            event_bus,
            ClusterBus::standalone(format!("proxy_{}", Uuid::new_v4().simple())),
        )
    }

    pub fn with_backplanes(event_bus: EventBus, cluster: ClusterBus) -> Self {
        Self::with_backplanes_and_traffic(event_bus, cluster, TrafficRecorder::default())
    }

    pub fn with_backplanes_and_traffic(
        event_bus: EventBus,
        cluster: ClusterBus,
        traffic: TrafficRecorder,
    ) -> Self {
        let (events, _) = broadcast::channel(512);
        Self {
            inner: std::sync::Arc::new(Inner {
                workspaces: RwLock::new(HashMap::new()),
                connections: RwLock::new(HashMap::new()),
                cluster_snapshot_revisions: Mutex::new(HashMap::new()),
                cluster_leases: Mutex::new(HashMap::new()),
                pending: Mutex::new(HashMap::new()),
                terminal_sessions: Mutex::new(HashMap::new()),
                network_streams: Mutex::new(HashMap::new()),
                browser_network_streams: Mutex::new(HashMap::new()),
                events,
                event_bus,
                cluster,
                traffic,
            }),
        }
    }

    pub async fn recent_machine_traffic(
        &self,
        workspace_id: &str,
        hours: u16,
    ) -> anyhow::Result<Vec<MachineTrafficRecord>> {
        self.inner.traffic.recent(workspace_id, hours).await
    }

    pub(crate) fn direct_traffic_meter(
        &self,
        workspace_id: &str,
        server_id: &str,
        host: &str,
        port: u16,
        agent_id: Option<&str>,
    ) -> crate::traffic::DirectTrafficMeter {
        self.inner
            .traffic
            .register_direct_stream(workspace_id, server_id, host, port)
            .with_agent_meter(agent_id.map(|agent| {
                self.inner.traffic.agent_view().register_direct_stream(
                    workspace_id,
                    agent,
                    host,
                    port,
                )
            }))
    }

    pub(crate) async fn issue_usage_ticket(
        &self,
        workspace: &str,
        server: &str,
        agent: Option<&str>,
        host: &str,
        port: u16,
    ) -> anyhow::Result<Option<String>> {
        self.inner
            .traffic
            .issue_usage_ticket(workspace, server, agent, host, port)
            .await
    }

    pub(crate) async fn abandon_usage_ticket(
        &self,
        workspace: &str,
        server: &str,
        ticket: &str,
    ) -> anyhow::Result<()> {
        self.inner
            .traffic
            .abandon_usage_ticket(workspace, server, ticket)
            .await
    }

    pub(crate) async fn persist_usage_report(
        &self,
        workspace: &str,
        server: &str,
        connection: Uuid,
        report: &treer_protocol::NetworkUsageReport,
    ) -> anyhow::Result<()> {
        self.require_current_connection(workspace, server, connection)
            .await
            .map_err(|error| anyhow::anyhow!("{}", error.message))?;
        self.inner
            .traffic
            .persist_usage_report(workspace, server, report)
            .await
    }

    pub async fn recent_agent_traffic(
        &self,
        workspace_id: &str,
        hours: u16,
    ) -> anyhow::Result<Vec<treer_protocol::AgentTrafficRecord>> {
        Ok(self
            .inner
            .traffic
            .agent_view()
            .recent(workspace_id, hours)
            .await?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    pub fn subscribe(&self) -> broadcast::Receiver<WorkspaceEvent> {
        self.inner.events.subscribe()
    }

    pub async fn allow_server_reenrollment(&self, workspace_id: &str, server_id: &str) {
        if let Some(workspace) = self.inner.workspaces.write().await.get_mut(workspace_id) {
            workspace.deleted_servers.remove(server_id);
        }
    }

    pub async fn broadcast_proxy_message(&self, workspace_id: &str, message: &ProxyMessage) {
        let Ok(encoded) = serde_json::to_string(message) else {
            return;
        };
        let frame = SocketFrame::Text(encoded);
        self.handle_cluster_workspace_broadcast(workspace_id, frame.clone())
            .await;
        if let Err(error) = self
            .inner
            .cluster
            .broadcast_workspace(workspace_id, frame)
            .await
        {
            warn!(?error, %workspace_id, "failed to broadcast workspace message across proxies");
        }
    }

    pub(crate) async fn handle_cluster_workspace_broadcast(
        &self,
        workspace_id: &str,
        frame: SocketFrame,
    ) {
        let outgoing = self
            .inner
            .connections
            .read()
            .await
            .iter()
            .filter(|(key, _)| key.workspace_id == workspace_id)
            .map(|(_, connection)| connection.outgoing.clone())
            .collect::<Vec<_>>();
        for connection in outgoing {
            let _ = connection.send(frame.clone());
        }
    }

    pub async fn ensure_workspace(&self, workspace_id: &str, name: &str) -> WorkspaceInfo {
        self.ensure_workspace_info(WorkspaceInfo {
            workspace_id: workspace_id.to_string(),
            name: name.to_string(),
            created_at: Utc::now(),
        })
        .await
    }

    pub async fn ensure_workspace_info(&self, info: WorkspaceInfo) -> WorkspaceInfo {
        self.upsert_workspace_info(info, false).await
    }

    pub(super) async fn upsert_workspace_info(
        &self,
        info: WorkspaceInfo,
        publish_change: bool,
    ) -> WorkspaceInfo {
        let mut event = None;
        let mut workspaces = self.inner.workspaces.write().await;
        let workspace = workspaces
            .entry(info.workspace_id.clone())
            .or_insert_with(|| WorkspaceState {
                info: info.clone(),
                revision: 0,
                servers: HashMap::new(),
                agents: HashMap::new(),
                server_names: HashMap::new(),
                agent_names: HashMap::new(),
                deleted_servers: HashSet::new(),
                deleted_agents: HashSet::new(),
            });
        if publish_change && workspace.info != info {
            workspace.info = info.clone();
            workspace.revision = workspace.revision.saturating_add(1);
            event = Some(WorkspaceEvent {
                revision: workspace.revision,
                workspace_id: info.workspace_id.clone(),
                event: "workspace.renamed".to_string(),
                data: serde_json::to_value(&info).unwrap_or(Value::Null),
            });
        }
        let current = workspace.info.clone();
        drop(workspaces);
        if let Some(event) = event {
            self.publish_workspace_event(event);
        }
        current
    }

    pub async fn create_workspace_info(
        &self,
        info: WorkspaceInfo,
    ) -> Result<WorkspaceInfo, ProtocolError> {
        {
            let mut workspaces = self.inner.workspaces.write().await;
            if workspaces.contains_key(&info.workspace_id) {
                return Err(ProtocolError::new(
                    "workspace_exists",
                    format!("workspace {} already exists", info.workspace_id),
                ));
            }
            workspaces.insert(
                info.workspace_id.clone(),
                WorkspaceState {
                    info: info.clone(),
                    revision: 0,
                    servers: HashMap::new(),
                    agents: HashMap::new(),
                    server_names: HashMap::new(),
                    agent_names: HashMap::new(),
                    deleted_servers: HashSet::new(),
                    deleted_agents: HashSet::new(),
                },
            );
        }
        self.broadcast_projection(ClusterProjectionUpdate::WorkspaceUpsert {
            workspace: info.clone(),
        })
        .await?;
        Ok(info)
    }

    pub async fn rename_workspace_info(
        &self,
        info: WorkspaceInfo,
    ) -> Result<WorkspaceInfo, ProtocolError> {
        self.upsert_workspace_info(info.clone(), true).await;
        self.broadcast_projection(ClusterProjectionUpdate::WorkspaceUpsert {
            workspace: info.clone(),
        })
        .await?;
        Ok(info)
    }

    pub async fn snapshot(&self, workspace_id: &str) -> Result<WorkspaceSnapshot, ProtocolError> {
        let workspaces = self.inner.workspaces.read().await;
        workspaces
            .get(workspace_id)
            .map(WorkspaceState::snapshot)
            .ok_or_else(|| {
                ProtocolError::new(
                    "workspace_not_found",
                    format!("workspace {workspace_id} does not exist"),
                )
            })
    }

    pub async fn platform_agent_count(&self) -> usize {
        self.inner
            .workspaces
            .read()
            .await
            .values()
            .map(|workspace| workspace.agents.len())
            .sum()
    }

    pub async fn live_servers(&self) -> Vec<ServerInfo> {
        let workspaces = self.inner.workspaces.read().await;
        let mut servers: Vec<_> = workspaces
            .values()
            .flat_map(|workspace| workspace.servers.values().cloned())
            .collect();
        servers.sort_by(|left, right| left.server_id.cmp(&right.server_id));
        servers
    }

    pub async fn live_agents(&self) -> Vec<AgentInfo> {
        let workspaces = self.inner.workspaces.read().await;
        let mut agents: Vec<_> = workspaces
            .values()
            .flat_map(|workspace| workspace.agents.values().cloned())
            .collect();
        agents.sort_by(|left, right| left.agent_id.cmp(&right.agent_id));
        agents
    }

    pub async fn live_server(&self, server_id: &str) -> Option<ServerInfo> {
        let workspaces = self.inner.workspaces.read().await;
        workspaces
            .values()
            .find_map(|workspace| workspace.servers.get(server_id).cloned())
    }

    #[cfg(test)]
    pub async fn test_insert_agent(&self, agent: AgentInfo) {
        let mut workspaces = self.inner.workspaces.write().await;
        workspaces
            .get_mut(&agent.workspace_id)
            .expect("workspace")
            .agents
            .insert(agent.agent_id.clone(), agent);
    }

    pub async fn live_agents_on_server(&self, server_id: &str) -> Vec<AgentInfo> {
        let workspaces = self.inner.workspaces.read().await;
        let mut agents: Vec<_> = workspaces
            .values()
            .flat_map(|workspace| workspace.agents.values())
            .filter(|agent| agent.server_id == server_id)
            .cloned()
            .collect();
        agents.sort_by(|left, right| left.agent_id.cmp(&right.agent_id));
        agents
    }

    #[cfg(test)]
    pub async fn register_server(
        &self,
        server: ServerInfo,
        connection_id: Uuid,
        outgoing: mpsc::UnboundedSender<SocketFrame>,
    ) -> Result<u64, ProtocolError> {
        self.register_server_instance(server, connection_id, "unknown".to_string(), outgoing)
            .await
    }

    #[cfg(test)]
    pub async fn register_server_instance(
        &self,
        server: ServerInfo,
        connection_id: Uuid,
        controller_instance_id: String,
        outgoing: mpsc::UnboundedSender<SocketFrame>,
    ) -> Result<u64, ProtocolError> {
        self.register_server_instance_with_capabilities(
            server,
            connection_id,
            controller_instance_id,
            treer_protocol::CONTROLLER_CAPABILITIES
                .iter()
                .map(|value| value.to_string()),
            outgoing,
        )
        .await
    }

    pub async fn register_server_instance_with_capabilities(
        &self,
        mut server: ServerInfo,
        connection_id: Uuid,
        controller_instance_id: String,
        capabilities: impl IntoIterator<Item = String>,
        outgoing: mpsc::UnboundedSender<SocketFrame>,
    ) -> Result<u64, ProtocolError> {
        self.ensure_workspace(&server.workspace_id, &server.workspace_id)
            .await;
        if self
            .inner
            .workspaces
            .read()
            .await
            .get(&server.workspace_id)
            .is_some_and(|workspace| workspace.deleted_servers.contains(&server.server_id))
        {
            return Err(ProtocolError::new(
                "server_deleted",
                format!("server {} was deleted", server.server_id),
            ));
        }
        let now = Utc::now();
        server.status = ServerStatus::Online;
        server.connected_at = now;
        server.last_seen_at = now;

        let key = ServerKey {
            workspace_id: server.workspace_id.clone(),
            server_id: server.server_id.clone(),
        };
        let replaced = self.inner.connections.write().await.insert(
            key,
            ServerConnection {
                connection_id,
                controller_instance_id: controller_instance_id.clone(),
                capabilities: capabilities.into_iter().collect(),
                outgoing,
            },
        );
        if let Some(replaced) = replaced {
            if let Ok(frame) = proxy_message_frame(&ProxyMessage::Error {
                error: ProtocolError::new(
                    "duplicate_machine_connection",
                    format!(
                        "Controller {controller_instance_id} replaced this connection for {}",
                        server.server_id
                    ),
                ),
            }) {
                let _ = replaced.outgoing.send(frame);
            }
            let _ = replaced.outgoing.send(SocketFrame::Close);
        }

        let event = self
            .mutate_workspace(
                &server.workspace_id,
                "server.updated",
                &server,
                |workspace| {
                    workspace
                        .servers
                        .insert(server.server_id.clone(), server.clone());
                },
            )
            .await?;
        let snapshot = self
            .server_snapshot(&server.workspace_id, &server.server_id)
            .await?;
        if let Err(error) = self
            .inner
            .cluster
            .claim(
                &server.workspace_id,
                &server.server_id,
                connection_id,
                snapshot,
            )
            .await
        {
            self.inner.connections.write().await.remove(&ServerKey {
                workspace_id: server.workspace_id.clone(),
                server_id: server.server_id.clone(),
            });
            return Err(error);
        }
        Ok(event.revision)
    }

    pub async fn apply_snapshot(
        &self,
        connection_id: Uuid,
        snapshot: AgentServerSnapshot,
    ) -> Result<(), ProtocolError> {
        self.require_current_connection(
            &snapshot.server.workspace_id,
            &snapshot.server.server_id,
            connection_id,
        )
        .await?;
        let mut server = snapshot.server;
        server.status = ServerStatus::Online;
        server.last_seen_at = Utc::now();
        let workspace_id = server.workspace_id.clone();
        let snapshot_workspace_id = workspace_id.clone();
        let server_id = server.server_id.clone();
        let agents = snapshot.agents;
        let event = {
            let mut workspaces = self.inner.workspaces.write().await;
            let workspace = workspaces
                .get_mut(&workspace_id)
                .ok_or_else(|| ProtocolError::new("workspace_not_found", &workspace_id))?;
            if workspace.deleted_servers.contains(&server_id) {
                return Err(ProtocolError::new("server_deleted", &server_id));
            }
            if let Some(current) = workspace.servers.get(&server_id) {
                server.name.clone_from(&current.name);
            }
            workspace.servers.insert(server_id.clone(), server.clone());
            let names: HashMap<_, _> = workspace
                .agents
                .values()
                .filter(|agent| agent.server_id == server_id)
                .map(|agent| (agent.agent_id.clone(), agent.name.clone()))
                .collect();
            workspace
                .agents
                .retain(|_, agent| agent.server_id != server_id);
            for mut agent in agents {
                if agent.workspace_id == snapshot_workspace_id && agent.server_id == server_id {
                    if workspace.deleted_agents.contains(&agent.agent_id) {
                        continue;
                    }
                    if let Some(name) = names.get(&agent.agent_id) {
                        agent.name.clone_from(name);
                    }
                    workspace.agents.insert(agent.agent_id.clone(), agent);
                }
            }
            workspace.revision = workspace.revision.saturating_add(1);
            WorkspaceEvent {
                revision: workspace.revision,
                workspace_id: workspace_id.clone(),
                event: "server.snapshot".to_string(),
                data: serde_json::to_value(&server).map_err(|error| {
                    ProtocolError::new("encode_error", format!("failed to encode event: {error}"))
                })?,
            }
        };
        self.publish_workspace_event(event);
        let snapshot = self.server_snapshot(&workspace_id, &server_id).await?;
        self.inner
            .cluster
            .publish_snapshot(&workspace_id, &server_id, connection_id, snapshot)
            .await?;
        self.resend_pending(&workspace_id, &server_id).await;
        Ok(())
    }

    pub async fn heartbeat(
        &self,
        workspace_id: &str,
        server_id: &str,
        connection_id: Uuid,
    ) -> Result<(), ProtocolError> {
        self.require_current_connection(workspace_id, server_id, connection_id)
            .await?;
        {
            let mut workspaces = self.inner.workspaces.write().await;
            let workspace = workspaces
                .get_mut(workspace_id)
                .ok_or_else(|| ProtocolError::new("workspace_not_found", workspace_id))?;
            let server = workspace
                .servers
                .get_mut(server_id)
                .ok_or_else(|| ProtocolError::new("server_not_found", server_id))?;
            server.last_seen_at = Utc::now();
            server.status = ServerStatus::Online;
        }
        if self
            .inner
            .cluster
            .renew(workspace_id, server_id, connection_id)
            .await?
        {
            Ok(())
        } else {
            Err(ProtocolError::new(
                "duplicate_machine_connection",
                format!(
                    "another Controller currently owns machine {server_id}; stopping this duplicate connection"
                ),
            ))
        }
    }

    pub async fn apply_agent_event(
        &self,
        connection_id: Uuid,
        mut agent: AgentInfo,
    ) -> Result<(), ProtocolError> {
        self.require_current_connection(&agent.workspace_id, &agent.server_id, connection_id)
            .await?;
        let workspace_id = agent.workspace_id.clone();
        let event = {
            let mut workspaces = self.inner.workspaces.write().await;
            let workspace = workspaces
                .get_mut(&workspace_id)
                .ok_or_else(|| ProtocolError::new("workspace_not_found", &workspace_id))?;
            if workspace.deleted_servers.contains(&agent.server_id) {
                return Err(ProtocolError::new("server_deleted", &agent.server_id));
            }
            if workspace.deleted_agents.contains(&agent.agent_id) {
                return Ok(());
            }
            if let Some(current) = workspace.agents.get(&agent.agent_id) {
                agent.name.clone_from(&current.name);
            }
            workspace
                .agents
                .insert(agent.agent_id.clone(), agent.clone());
            workspace.revision = workspace.revision.saturating_add(1);
            WorkspaceEvent {
                revision: workspace.revision,
                workspace_id,
                event: "agent.updated".to_string(),
                data: serde_json::to_value(&agent).map_err(|error| {
                    ProtocolError::new("encode_error", format!("failed to encode event: {error}"))
                })?,
            }
        };
        self.publish_workspace_event(event);
        let snapshot = self
            .server_snapshot(&agent.workspace_id, &agent.server_id)
            .await?;
        self.inner
            .cluster
            .publish_snapshot(
                &agent.workspace_id,
                &agent.server_id,
                connection_id,
                snapshot,
            )
            .await?;
        Ok(())
    }

    pub async fn restore_deleted_agents(
        &self,
        workspace_id: &str,
        agent_ids: impl IntoIterator<Item = String>,
    ) -> Result<(), ProtocolError> {
        let mut workspaces = self.inner.workspaces.write().await;
        let workspace = workspaces
            .get_mut(workspace_id)
            .ok_or_else(|| ProtocolError::new("workspace_not_found", workspace_id))?;
        for agent_id in agent_ids {
            workspace.agents.remove(&agent_id);
            workspace.agent_names.remove(&agent_id);
            workspace.deleted_agents.insert(agent_id);
        }
        Ok(())
    }

    pub async fn delete_agent(
        &self,
        workspace_id: &str,
        agent_id: &str,
    ) -> Result<AgentInfo, ProtocolError> {
        let (agent, event) = {
            let mut workspaces = self.inner.workspaces.write().await;
            let workspace = workspaces
                .get_mut(workspace_id)
                .ok_or_else(|| ProtocolError::new("workspace_not_found", workspace_id))?;
            let agent = workspace
                .agents
                .remove(agent_id)
                .ok_or_else(|| ProtocolError::new("agent_not_found", agent_id))?;
            workspace.agent_names.remove(agent_id);
            workspace.deleted_agents.insert(agent_id.to_string());
            workspace.revision = workspace.revision.saturating_add(1);
            let event = WorkspaceEvent {
                revision: workspace.revision,
                workspace_id: workspace_id.to_string(),
                event: "agent.deleted".to_string(),
                data: serde_json::to_value(&agent).map_err(|error| {
                    ProtocolError::new("encode_error", format!("failed to encode event: {error}"))
                })?,
            };
            (agent, event)
        };
        self.publish_workspace_event(event);
        self.broadcast_projection(ClusterProjectionUpdate::AgentDeleted {
            workspace_id: workspace_id.to_string(),
            agent_id: agent_id.to_string(),
        })
        .await?;
        self.close_agent_terminals(workspace_id, agent_id).await;
        Ok(agent)
    }

    pub async fn resolve_server(
        &self,
        workspace_id: &str,
        target: &str,
    ) -> Result<ServerInfo, ProtocolError> {
        let workspaces = self.inner.workspaces.read().await;
        let workspace = workspaces
            .get(workspace_id)
            .ok_or_else(|| ProtocolError::new("workspace_not_found", workspace_id))?;
        if let Some(server) = workspace.servers.get(target) {
            return Ok(server.clone());
        }
        let mut matches = workspace
            .servers
            .values()
            .filter(|server| server.name == target);
        let Some(server) = matches.next() else {
            return Err(ProtocolError::new("server_not_found", target));
        };
        if matches.next().is_some() {
            return Err(ProtocolError::new(
                "server_ambiguous",
                format!("more than one machine is named {target}; use a server id"),
            ));
        }
        Ok(server.clone())
    }

    pub async fn rename_server(
        &self,
        workspace_id: &str,
        server_id: &str,
        name: String,
    ) -> Result<ServerInfo, ProtocolError> {
        let (server, event) = {
            let mut workspaces = self.inner.workspaces.write().await;
            let workspace = workspaces
                .get_mut(workspace_id)
                .ok_or_else(|| ProtocolError::new("workspace_not_found", workspace_id))?;
            let server = workspace
                .servers
                .get_mut(server_id)
                .ok_or_else(|| ProtocolError::new("server_not_found", server_id))?;
            server.name = name;
            let server = server.clone();
            workspace
                .server_names
                .insert(server_id.to_string(), server.name.clone());
            workspace.revision = workspace.revision.saturating_add(1);
            let event = WorkspaceEvent {
                revision: workspace.revision,
                workspace_id: workspace_id.to_string(),
                event: "server.renamed".to_string(),
                data: serde_json::to_value(&server).map_err(|error| {
                    ProtocolError::new("encode_error", format!("failed to encode event: {error}"))
                })?,
            };
            (server, event)
        };
        self.publish_workspace_event(event);
        self.broadcast_projection(ClusterProjectionUpdate::ServerRenamed {
            workspace_id: workspace_id.to_string(),
            server_id: server_id.to_string(),
            name: server.name.clone(),
        })
        .await?;
        Ok(server)
    }

    pub async fn delete_server(
        &self,
        workspace_id: &str,
        server_id: &str,
    ) -> Result<(ServerInfo, Vec<AgentInfo>), ProtocolError> {
        let (server, agents, event) = {
            let mut workspaces = self.inner.workspaces.write().await;
            let workspace = workspaces
                .get_mut(workspace_id)
                .ok_or_else(|| ProtocolError::new("workspace_not_found", workspace_id))?;
            let server = workspace
                .servers
                .remove(server_id)
                .ok_or_else(|| ProtocolError::new("server_not_found", server_id))?;
            workspace.server_names.remove(server_id);
            workspace.deleted_servers.insert(server_id.to_string());
            let agent_ids = workspace
                .agents
                .values()
                .filter(|agent| agent.server_id == server_id)
                .map(|agent| agent.agent_id.clone())
                .collect::<Vec<_>>();
            let agents = agent_ids
                .iter()
                .filter_map(|agent_id| workspace.agents.remove(agent_id))
                .collect::<Vec<_>>();
            for agent_id in &agent_ids {
                workspace.agent_names.remove(agent_id);
            }
            workspace.deleted_agents.extend(agent_ids.iter().cloned());
            workspace.revision = workspace.revision.saturating_add(1);
            let event = WorkspaceEvent {
                revision: workspace.revision,
                workspace_id: workspace_id.to_string(),
                event: "server.deleted".to_string(),
                data: serde_json::json!({
                    "server": server,
                    "agent_ids": agent_ids,
                }),
            };
            (server, agents, event)
        };
        self.publish_workspace_event(event);
        self.release_server_runtime(workspace_id, server_id, "machine deleted", "server_deleted")
            .await;
        self.broadcast_projection(ClusterProjectionUpdate::ServerDeleted {
            workspace_id: workspace_id.to_string(),
            server_id: server_id.to_string(),
        })
        .await?;

        Ok((server, agents))
    }

    pub async fn delete_workspace(&self, workspace_id: &str) -> Result<(), ProtocolError> {
        let removed = {
            let mut workspaces = self.inner.workspaces.write().await;
            workspaces.remove(workspace_id)
        };
        if let Some(workspace) = removed {
            self.publish_workspace_event(WorkspaceEvent {
                revision: workspace.revision.saturating_add(1),
                workspace_id: workspace_id.to_string(),
                event: "workspace.deleted".to_string(),
                data: serde_json::to_value(&workspace.info).unwrap_or(Value::Null),
            });
            for server_id in workspace.servers.keys() {
                self.release_server_runtime(
                    workspace_id,
                    server_id,
                    "workspace deleted",
                    "workspace_deleted",
                )
                .await;
            }
        }
        self.broadcast_projection(ClusterProjectionUpdate::WorkspaceDeleted {
            workspace_id: workspace_id.to_string(),
        })
        .await?;
        Ok(())
    }

    pub(super) async fn release_server_runtime(
        &self,
        workspace_id: &str,
        server_id: &str,
        terminal_reason: &str,
        pending_error: &str,
    ) {
        let key = ServerKey {
            workspace_id: workspace_id.to_string(),
            server_id: server_id.to_string(),
        };
        let connection = self.inner.connections.write().await.remove(&key);
        if let Some(connection) = connection {
            self.inner
                .cluster
                .release(workspace_id, server_id, connection.connection_id)
                .await;
            let _ = connection.outgoing.send(SocketFrame::Close);
        }

        let cancelled = {
            let mut pending = self.inner.pending.lock().await;
            let command_ids = pending
                .iter()
                .filter(|(_, command)| command.server == key)
                .map(|(command_id, _)| command_id.clone())
                .collect::<Vec<_>>();
            command_ids
                .into_iter()
                .filter_map(|command_id| {
                    pending
                        .remove(&command_id)
                        .map(|command| (command_id, command))
                })
                .collect::<Vec<_>>()
        };
        for (command_id, command) in cancelled {
            let _ = command.result.send(CommandResult::failure(
                command_id,
                ProtocolError::new(pending_error, server_id),
            ));
        }

        let terminals = {
            let mut sessions = self.inner.terminal_sessions.lock().await;
            let session_ids = sessions
                .iter()
                .filter(|(_, session)| {
                    session.workspace_id == workspace_id && session.server_id == server_id
                })
                .map(|(session_id, _)| session_id.clone())
                .collect::<Vec<_>>();
            session_ids
                .into_iter()
                .filter_map(|session_id| sessions.remove(&session_id))
                .collect::<Vec<_>>()
        };
        for terminal in terminals {
            send_terminal_to_browser(
                &terminal.outgoing,
                &TerminalServerMessage::Closed {
                    reason: Some(terminal_reason.to_string()),
                    exit_code: None,
                },
            );
        }
        self.close_server_network_streams(workspace_id, server_id)
            .await;
    }

    pub async fn rename_agent(
        &self,
        workspace_id: &str,
        agent_id: &str,
        name: String,
    ) -> Result<AgentInfo, ProtocolError> {
        let (agent, event) = {
            let mut workspaces = self.inner.workspaces.write().await;
            let workspace = workspaces
                .get_mut(workspace_id)
                .ok_or_else(|| ProtocolError::new("workspace_not_found", workspace_id))?;
            let agent = workspace
                .agents
                .get_mut(agent_id)
                .ok_or_else(|| ProtocolError::new("agent_not_found", agent_id))?;
            agent.name = name;
            agent.updated_at = Utc::now();
            let agent = agent.clone();
            workspace
                .agent_names
                .insert(agent_id.to_string(), agent.name.clone());
            workspace.revision = workspace.revision.saturating_add(1);
            let event = WorkspaceEvent {
                revision: workspace.revision,
                workspace_id: workspace_id.to_string(),
                event: "agent.renamed".to_string(),
                data: serde_json::to_value(&agent).map_err(|error| {
                    ProtocolError::new("encode_error", format!("failed to encode event: {error}"))
                })?,
            };
            (agent, event)
        };
        self.publish_workspace_event(event);
        self.broadcast_projection(ClusterProjectionUpdate::AgentRenamed {
            workspace_id: workspace_id.to_string(),
            agent_id: agent_id.to_string(),
            name: agent.name.clone(),
        })
        .await?;
        Ok(agent)
    }

    pub async fn resolve_agent(
        &self,
        workspace_id: &str,
        target: &str,
    ) -> Result<AgentInfo, ProtocolError> {
        let workspaces = self.inner.workspaces.read().await;
        let workspace = workspaces
            .get(workspace_id)
            .ok_or_else(|| ProtocolError::new("workspace_not_found", workspace_id))?;
        if let Some(agent) = workspace.agents.get(target) {
            return Ok(agent.clone());
        }
        let mut matches = workspace
            .agents
            .values()
            .filter(|agent| agent.name == target);
        let Some(agent) = matches.next() else {
            return Err(ProtocolError::new("agent_not_found", target));
        };
        if matches.next().is_some() {
            return Err(ProtocolError::new(
                "agent_ambiguous",
                format!("more than one agent is named {target}; use an agent id"),
            ));
        }
        Ok(agent.clone())
    }

    pub async fn select_server(
        &self,
        workspace_id: &str,
        requested: Option<&str>,
    ) -> Result<String, ProtocolError> {
        let workspaces = self.inner.workspaces.read().await;
        let workspace = workspaces
            .get(workspace_id)
            .ok_or_else(|| ProtocolError::new("workspace_not_found", workspace_id))?;
        if let Some(server_id) = requested {
            let server = workspace
                .servers
                .get(server_id)
                .ok_or_else(|| ProtocolError::new("server_not_found", server_id))?;
            if server.status != ServerStatus::Online {
                return Err(ProtocolError::new("server_offline", server_id));
            }
            return Ok(server_id.to_string());
        }
        let mut candidates: Vec<_> = workspace
            .servers
            .values()
            .filter(|server| server.status == ServerStatus::Online)
            .map(|server| server.server_id.clone())
            .collect();
        candidates.sort();
        candidates.into_iter().next().ok_or_else(|| {
            ProtocolError::new(
                "no_online_server",
                format!("workspace {workspace_id} has no online agent server"),
            )
        })
    }
}
