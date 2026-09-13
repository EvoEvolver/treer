use super::*;

impl AppState {
    pub(super) async fn server_outgoing(
        &self,
        workspace_id: &str,
        server_id: &str,
    ) -> Option<mpsc::UnboundedSender<SocketFrame>> {
        let key = ServerKey {
            workspace_id: workspace_id.to_string(),
            server_id: server_id.to_string(),
        };
        self.inner
            .connections
            .read()
            .await
            .get(&key)
            .map(|connection| connection.outgoing.clone())
    }

    pub(super) async fn send_server_frame(
        &self,
        workspace_id: &str,
        server_id: &str,
        frame: SocketFrame,
    ) -> Result<(), ProtocolError> {
        if !self.inner.cluster.is_distributed() {
            return self
                .local_server_frame(workspace_id, server_id, None, frame)
                .await;
        }
        let owner = self
            .inner
            .cluster
            .owner(workspace_id, server_id)
            .await?
            .ok_or_else(|| ProtocolError::new("server_offline", server_id))?;
        if owner.proxy_id == self.inner.cluster.instance_id() {
            self.local_server_frame(workspace_id, server_id, Some(owner.connection_id), frame)
                .await
        } else {
            self.inner
                .cluster
                .send_socket(&owner, workspace_id, server_id, frame)
                .await
        }
    }

    pub(super) async fn local_server_frame(
        &self,
        workspace_id: &str,
        server_id: &str,
        expected_connection_id: Option<Uuid>,
        frame: SocketFrame,
    ) -> Result<(), ProtocolError> {
        let key = ServerKey {
            workspace_id: workspace_id.to_string(),
            server_id: server_id.to_string(),
        };
        let outgoing = self
            .inner
            .connections
            .read()
            .await
            .get(&key)
            .filter(|connection| {
                expected_connection_id.is_none_or(|expected| expected == connection.connection_id)
            })
            .map(|connection| connection.outgoing.clone())
            .ok_or_else(|| ProtocolError::new("server_offline", server_id))?;
        outgoing
            .send(frame)
            .map_err(|_| ProtocolError::new("server_offline", server_id))
    }

    pub(crate) async fn handle_cluster_socket(
        &self,
        workspace_id: &str,
        server_id: &str,
        connection_id: Uuid,
        frame: SocketFrame,
    ) -> Result<(), ProtocolError> {
        self.local_server_frame(workspace_id, server_id, Some(connection_id), frame)
            .await
    }

    pub(super) async fn server_snapshot(
        &self,
        workspace_id: &str,
        server_id: &str,
    ) -> Result<AgentServerSnapshot, ProtocolError> {
        let workspaces = self.inner.workspaces.read().await;
        let workspace = workspaces
            .get(workspace_id)
            .ok_or_else(|| ProtocolError::new("workspace_not_found", workspace_id))?;
        let server = workspace
            .servers
            .get(server_id)
            .cloned()
            .ok_or_else(|| ProtocolError::new("server_not_found", server_id))?;
        let agents = workspace
            .agents
            .values()
            .filter(|agent| agent.server_id == server_id)
            .cloned()
            .collect();
        Ok(AgentServerSnapshot { server, agents })
    }

    pub(crate) async fn apply_cluster_snapshot(&self, update: ClusterServerSnapshot) {
        let key = ServerKey {
            workspace_id: update.snapshot.server.workspace_id.clone(),
            server_id: update.snapshot.server.server_id.clone(),
        };
        {
            let mut revisions = self.inner.cluster_snapshot_revisions.lock().await;
            if revisions
                .get(&key)
                .is_some_and(|revision| *revision >= update.revision)
            {
                return;
            }
            revisions.insert(key, update.revision);
        }
        let snapshot = update.snapshot;
        let workspace_id = snapshot.server.workspace_id.clone();
        let server_id = snapshot.server.server_id.clone();
        self.ensure_workspace(&workspace_id, &workspace_id).await;
        let event = {
            let mut workspaces = self.inner.workspaces.write().await;
            let Some(workspace) = workspaces.get_mut(&workspace_id) else {
                return;
            };
            workspace.deleted_servers.remove(&server_id);
            let mut server = snapshot.server;
            if let Some(name) = workspace.server_names.get(&server_id) {
                server.name.clone_from(name);
            } else if let Some(current) = workspace.servers.get(&server_id) {
                server.name.clone_from(&current.name);
            }
            workspace.servers.insert(server_id.clone(), server);
            let names = workspace
                .agents
                .values()
                .filter(|agent| agent.server_id == server_id)
                .map(|agent| (agent.agent_id.clone(), agent.name.clone()))
                .collect::<HashMap<_, _>>();
            workspace
                .agents
                .retain(|_, agent| agent.server_id != server_id);
            for mut agent in snapshot.agents {
                if !workspace.deleted_agents.contains(&agent.agent_id) {
                    if let Some(name) = workspace.agent_names.get(&agent.agent_id) {
                        agent.name.clone_from(name);
                    } else if let Some(name) = names.get(&agent.agent_id) {
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
                data: serde_json::json!({ "server_id": server_id }),
            }
        };
        let _ = self.inner.events.send(event);
    }

    pub(crate) async fn apply_cluster_disconnect(
        &self,
        workspace_id: &str,
        server_id: &str,
        revision: u64,
    ) {
        if self
            .server_outgoing(workspace_id, server_id)
            .await
            .is_some()
        {
            return;
        }
        let key = ServerKey {
            workspace_id: workspace_id.to_string(),
            server_id: server_id.to_string(),
        };
        {
            let mut leases = self.inner.cluster_leases.lock().await;
            if leases
                .get(&key)
                .is_some_and(|(current, _)| *current > revision)
            {
                return;
            }
            leases.insert(key.clone(), (revision, None));
        }
        let event = {
            let mut workspaces = self.inner.workspaces.write().await;
            let Some(workspace) = workspaces.get_mut(workspace_id) else {
                return;
            };
            let Some(server) = workspace.servers.get_mut(server_id) else {
                return;
            };
            if server.status == ServerStatus::Offline {
                None
            } else {
                server.status = ServerStatus::Offline;
                server.last_seen_at = Utc::now();
                workspace.revision = workspace.revision.saturating_add(1);
                Some(WorkspaceEvent {
                    revision: workspace.revision,
                    workspace_id: workspace_id.to_string(),
                    event: "server.offline".to_string(),
                    data: serde_json::json!({ "server_id": server_id, "status": "offline" }),
                })
            }
        };
        if let Some(event) = event {
            let _ = self.inner.events.send(event);
        }
        self.close_server_sessions(workspace_id, server_id, "agent server disconnected")
            .await;
    }

    pub(crate) async fn note_cluster_lease(
        &self,
        workspace_id: &str,
        server_id: &str,
        revision: u64,
    ) {
        let key = ServerKey {
            workspace_id: workspace_id.to_string(),
            server_id: server_id.to_string(),
        };
        let accepted = {
            let mut leases = self.inner.cluster_leases.lock().await;
            if leases
                .get(&key)
                .is_some_and(|(current, _)| revision < *current)
            {
                false
            } else {
                leases.insert(key, (revision, Some(Instant::now())));
                true
            }
        };
        if !accepted {
            return;
        }
        let event = {
            let mut workspaces = self.inner.workspaces.write().await;
            let Some(workspace) = workspaces.get_mut(workspace_id) else {
                return;
            };
            let Some(server) = workspace.servers.get_mut(server_id) else {
                return;
            };
            if server.status == ServerStatus::Online {
                return;
            }
            server.status = ServerStatus::Online;
            server.last_seen_at = Utc::now();
            workspace.revision = workspace.revision.saturating_add(1);
            WorkspaceEvent {
                revision: workspace.revision,
                workspace_id: workspace_id.to_string(),
                event: "server.online".to_string(),
                data: serde_json::json!({ "server_id": server_id, "status": "online" }),
            }
        };
        let _ = self.inner.events.send(event);
    }

    pub(super) async fn expire_cluster_lease_entries(
        &self,
        max_age: Duration,
    ) -> Vec<(ServerKey, u64)> {
        let mut leases = self.inner.cluster_leases.lock().await;
        let expired = leases
            .iter()
            .filter(|(_, (_, seen_at))| seen_at.is_some_and(|seen_at| seen_at.elapsed() >= max_age))
            .map(|(key, (revision, _))| (key.clone(), *revision))
            .collect::<Vec<_>>();
        for (key, revision) in &expired {
            leases.insert(key.clone(), (*revision, None));
        }
        expired
    }

    pub(crate) async fn expire_cluster_leases(&self, max_age: Duration) {
        let expired = self.expire_cluster_lease_entries(max_age).await;
        for (key, revision) in expired {
            self.apply_cluster_disconnect(&key.workspace_id, &key.server_id, revision)
                .await;
        }
    }

    pub(super) async fn broadcast_projection(
        &self,
        update: ClusterProjectionUpdate,
    ) -> Result<(), ProtocolError> {
        self.inner.cluster.broadcast_projection(update).await
    }

    pub(crate) async fn apply_cluster_projection(&self, update: ClusterProjectionUpdate) {
        match update {
            ClusterProjectionUpdate::WorkspaceUpsert { workspace } => {
                self.upsert_workspace_info(workspace, true).await;
            }
            ClusterProjectionUpdate::WorkspaceDeleted { workspace_id } => {
                let (event, server_ids) = {
                    let mut workspaces = self.inner.workspaces.write().await;
                    let Some(workspace) = workspaces.remove(&workspace_id) else {
                        return;
                    };
                    let event = Some(WorkspaceEvent {
                        revision: workspace.revision.saturating_add(1),
                        workspace_id: workspace_id.clone(),
                        event: "workspace.deleted".to_string(),
                        data: serde_json::to_value(&workspace.info).unwrap_or(Value::Null),
                    });
                    let server_ids = workspace.servers.keys().cloned().collect::<Vec<_>>();
                    (event, server_ids)
                };
                if let Some(event) = event {
                    self.publish_workspace_event(event);
                }
                for server_id in server_ids {
                    self.release_server_runtime(
                        &workspace_id,
                        &server_id,
                        "workspace deleted",
                        "workspace_deleted",
                    )
                    .await;
                }
            }
            ClusterProjectionUpdate::ServerRenamed {
                workspace_id,
                server_id,
                name,
            } => {
                let event = {
                    let mut workspaces = self.inner.workspaces.write().await;
                    let Some(workspace) = workspaces.get_mut(&workspace_id) else {
                        return;
                    };
                    workspace.deleted_servers.remove(&server_id);
                    workspace
                        .server_names
                        .insert(server_id.clone(), name.clone());
                    let Some(server) = workspace.servers.get_mut(&server_id) else {
                        return;
                    };
                    if server.name == name {
                        return;
                    }
                    server.name = name;
                    workspace.revision = workspace.revision.saturating_add(1);
                    WorkspaceEvent {
                        revision: workspace.revision,
                        workspace_id: workspace_id.clone(),
                        event: "server.renamed".to_string(),
                        data: serde_json::json!({ "server_id": server_id }),
                    }
                };
                let _ = self.inner.events.send(event);
            }
            ClusterProjectionUpdate::AgentRenamed {
                workspace_id,
                agent_id,
                name,
            } => {
                let event = {
                    let mut workspaces = self.inner.workspaces.write().await;
                    let Some(workspace) = workspaces.get_mut(&workspace_id) else {
                        return;
                    };
                    workspace.deleted_agents.remove(&agent_id);
                    workspace.agent_names.insert(agent_id.clone(), name.clone());
                    let Some(agent) = workspace.agents.get_mut(&agent_id) else {
                        return;
                    };
                    if agent.name == name {
                        return;
                    }
                    agent.name = name;
                    agent.updated_at = Utc::now();
                    workspace.revision = workspace.revision.saturating_add(1);
                    WorkspaceEvent {
                        revision: workspace.revision,
                        workspace_id: workspace_id.clone(),
                        event: "agent.renamed".to_string(),
                        data: serde_json::json!({ "agent_id": agent_id }),
                    }
                };
                let _ = self.inner.events.send(event);
            }
            ClusterProjectionUpdate::AgentDeleted {
                workspace_id,
                agent_id,
            } => {
                let event = {
                    let mut workspaces = self.inner.workspaces.write().await;
                    let Some(workspace) = workspaces.get_mut(&workspace_id) else {
                        return;
                    };
                    let removed = workspace.agents.remove(&agent_id).is_some();
                    workspace.agent_names.remove(&agent_id);
                    let inserted = workspace.deleted_agents.insert(agent_id.clone());
                    if !removed && !inserted {
                        return;
                    }
                    workspace.revision = workspace.revision.saturating_add(1);
                    WorkspaceEvent {
                        revision: workspace.revision,
                        workspace_id: workspace_id.clone(),
                        event: "agent.deleted".to_string(),
                        data: serde_json::json!({ "agent_id": agent_id }),
                    }
                };
                let _ = self.inner.events.send(event);
                self.close_agent_terminals(&workspace_id, &agent_id).await;
            }
            ClusterProjectionUpdate::ServerDeleted {
                workspace_id,
                server_id,
            } => {
                let event = {
                    let mut workspaces = self.inner.workspaces.write().await;
                    let Some(workspace) = workspaces.get_mut(&workspace_id) else {
                        return;
                    };
                    let removed = workspace.servers.remove(&server_id).is_some();
                    workspace.server_names.remove(&server_id);
                    let inserted = workspace.deleted_servers.insert(server_id.clone());
                    if !removed && !inserted {
                        return;
                    }
                    workspace
                        .agents
                        .retain(|_, agent| agent.server_id != server_id);
                    workspace.revision = workspace.revision.saturating_add(1);
                    WorkspaceEvent {
                        revision: workspace.revision,
                        workspace_id: workspace_id.clone(),
                        event: "server.deleted".to_string(),
                        data: serde_json::json!({ "server_id": server_id }),
                    }
                };
                let _ = self.inner.events.send(event);
                let key = ServerKey {
                    workspace_id: workspace_id.clone(),
                    server_id: server_id.clone(),
                };
                let connection = self.inner.connections.write().await.remove(&key);
                if let Some(connection) = connection {
                    self.inner
                        .cluster
                        .release(&workspace_id, &server_id, connection.connection_id)
                        .await;
                    let _ = connection.outgoing.send(SocketFrame::Close);
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
                            reason: Some("machine deleted".to_string()),
                            exit_code: None,
                        },
                    );
                }
                self.close_server_network_streams(&workspace_id, &server_id)
                    .await;
            }
        }
    }

    pub(crate) async fn handle_cluster_session_delivery(
        &self,
        delivery: ClusterSessionDelivery,
    ) -> Result<(), ProtocolError> {
        let overloaded_route = {
            let mut sessions = self.inner.terminal_sessions.lock().await;
            let session = sessions
                .get_mut(&delivery.session_id)
                .ok_or_else(|| ProtocolError::new("terminal_not_found", &delivery.session_id))?;
            if session.workspace_id != delivery.workspace_id
                || session.server_id != delivery.server_id
            {
                return Err(ProtocolError::new(
                    "terminal_identity_mismatch",
                    &delivery.session_id,
                ));
            }
            // The browser's session can belong to a different Proxy from the
            // Controller connection. Learn the epoch at the delivery owner so
            // subsequent binary output can include its reconnect cursor.
            if let SocketFrame::Text(text) = &delivery.frame {
                if let Ok(TerminalServerMessage::Ready {
                    session_id,
                    stream_epoch,
                    ..
                }) = serde_json::from_str(text)
                {
                    if session_id != delivery.session_id {
                        return Err(ProtocolError::new(
                            "terminal_identity_mismatch",
                            &delivery.session_id,
                        ));
                    }
                    session.stream_epoch = stream_epoch;
                }
            }
            if delivery.revision.is_some_and(|revision| {
                session
                    .last_revision
                    .is_some_and(|last_revision| revision <= last_revision)
            }) {
                return Ok(());
            }
            let cursor_frame = if delivery.cursor {
                session
                    .stream_epoch
                    .as_ref()
                    .zip(delivery.revision)
                    .map(|(stream_epoch, revision)| {
                        serde_json::to_string(&TerminalServerMessage::Cursor {
                            stream_epoch: stream_epoch.clone(),
                            revision,
                        })
                        .map(SocketFrame::Text)
                        .map_err(|error| ProtocolError::new("encode_error", error.to_string()))
                    })
                    .transpose()?
            } else {
                None
            };
            let route = (session.workspace_id.clone(), session.server_id.clone());
            let send_result = session.outgoing.try_send(delivery.frame).and_then(|_| {
                if let Some(cursor_frame) = cursor_frame {
                    session.outgoing.try_send(cursor_frame)
                } else {
                    Ok(())
                }
            });
            match send_result {
                Ok(()) => {
                    if let Some(revision) = delivery.revision {
                        session.last_revision = Some(revision);
                    }
                    if delivery.close {
                        sessions.remove(&delivery.session_id);
                    }
                    None
                }
                Err(mpsc::error::TrySendError::Full(_)) => {
                    sessions.remove(&delivery.session_id);
                    Some(route)
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    sessions.remove(&delivery.session_id);
                    return Err(ProtocolError::new("terminal_closed", delivery.session_id));
                }
            }
        };
        if let Some((workspace_id, server_id)) = overloaded_route {
            let message = ProxyMessage::TerminalDetach {
                session_id: delivery.session_id,
            };
            if let Ok(frame) = proxy_message_frame(&message) {
                let _ = self
                    .send_server_frame(&workspace_id, &server_id, frame)
                    .await;
            }
        }
        Ok(())
    }

    pub(super) async fn terminal_session_route(
        &self,
        session_id: &str,
    ) -> Result<(String, String), ProtocolError> {
        self.inner
            .terminal_sessions
            .lock()
            .await
            .get(session_id)
            .map(|session| (session.workspace_id.clone(), session.server_id.clone()))
            .ok_or_else(|| ProtocolError::new("terminal_not_found", session_id))
    }

    pub(super) async fn relay_terminal(
        &self,
        workspace_id: &str,
        server_id: &str,
        session_id: &str,
        message: TerminalServerMessage,
        close: bool,
    ) -> Result<(), ProtocolError> {
        let encoded = serde_json::to_string(&message)
            .map_err(|error| ProtocolError::new("encode_error", error.to_string()))?;
        self.deliver_session_frame(ClusterSessionDelivery {
            kind: ClusterSessionKind::Terminal,
            workspace_id: workspace_id.to_string(),
            server_id: server_id.to_string(),
            session_id: session_id.to_string(),
            revision: None,
            cursor: false,
            close,
            frame: SocketFrame::Text(encoded),
        })
        .await
    }

    pub(super) async fn deliver_session_frame(
        &self,
        delivery: ClusterSessionDelivery,
    ) -> Result<(), ProtocolError> {
        let local = self
            .inner
            .terminal_sessions
            .lock()
            .await
            .contains_key(&delivery.session_id);
        if local {
            return self.handle_cluster_session_delivery(delivery).await;
        }
        let target = ClusterBus::route_target(&delivery.session_id)
            .ok_or_else(|| ProtocolError::new("session_not_found", &delivery.session_id))?;
        if target == self.inner.cluster.instance_id() {
            return Err(ProtocolError::new("session_not_found", delivery.session_id));
        }
        self.inner.cluster.deliver_session(&target, delivery).await
    }

    pub(super) async fn require_current_connection(
        &self,
        workspace_id: &str,
        server_id: &str,
        connection_id: Uuid,
    ) -> Result<(), ProtocolError> {
        let key = ServerKey {
            workspace_id: workspace_id.to_string(),
            server_id: server_id.to_string(),
        };
        let connections = self.inner.connections.read().await;
        match connections.get(&key) {
            Some(connection) if connection.connection_id == connection_id => Ok(()),
            Some(connection) => Err(ProtocolError::new(
                "duplicate_machine_connection",
                format!(
                    "Controller {} currently owns machine {server_id}",
                    connection.controller_instance_id
                ),
            )),
            None => Err(ProtocolError::new(
                "stale_connection",
                format!("connection for {server_id} is no longer current"),
            )),
        }
    }

    pub(super) async fn mutate_workspace<T: Serialize>(
        &self,
        workspace_id: &str,
        event_name: &str,
        payload: &T,
        mutation: impl FnOnce(&mut WorkspaceState),
    ) -> Result<WorkspaceEvent, ProtocolError> {
        let event = {
            let mut workspaces = self.inner.workspaces.write().await;
            let workspace = workspaces
                .get_mut(workspace_id)
                .ok_or_else(|| ProtocolError::new("workspace_not_found", workspace_id))?;
            mutation(workspace);
            workspace.revision = workspace.revision.saturating_add(1);
            WorkspaceEvent {
                revision: workspace.revision,
                workspace_id: workspace_id.to_string(),
                event: event_name.to_string(),
                data: serde_json::to_value(payload).map_err(|err| {
                    ProtocolError::new("encode_error", format!("failed to encode event: {err}"))
                })?,
            }
        };
        self.publish_workspace_event(event.clone());
        Ok(event)
    }

    pub(super) fn publish_workspace_event(&self, event: WorkspaceEvent) {
        let _ = self.inner.events.send(event.clone());
        let envelope = DomainEventEnvelope {
            event_id: format!("evt_{}", Uuid::new_v4().simple()),
            schema_version: DOMAIN_EVENT_SCHEMA_VERSION,
            organization_id: None,
            workspace_id: event.workspace_id.clone(),
            actor: DomainEventActor {
                kind: "system".to_string(),
                id: Some("treer-proxy".to_string()),
            },
            action: event.event,
            resource: DomainEventResource {
                kind: "workspace".to_string(),
                id: event.workspace_id,
            },
            occurred_at: Utc::now(),
            trace_id: None,
            causation_id: None,
            correlation_id: None,
            workspace_revision: Some(event.revision),
            payload: event.data,
        };
        if let Err(error) = self.inner.event_bus.publish(envelope) {
            warn!(%error, "domain event could not be queued");
        }
    }
}
