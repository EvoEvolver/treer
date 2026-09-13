use super::*;

impl AppState {
    pub async fn attach_terminal(
        &self,
        workspace_id: &str,
        agent_id: &str,
        cols: u16,
        rows: u16,
        cursor: Option<TerminalCursor>,
        outgoing: mpsc::Sender<SocketFrame>,
    ) -> Result<String, ProtocolError> {
        let agent = self.resolve_agent(workspace_id, agent_id).await?;
        let server_id = agent.server_id;
        let session_id = self.inner.cluster.routed_id("term");
        self.inner.terminal_sessions.lock().await.insert(
            session_id.clone(),
            TerminalSession {
                workspace_id: workspace_id.to_string(),
                server_id: server_id.clone(),
                process_id: agent.agent_id.clone(),
                outgoing,
                last_revision: None,
                stream_epoch: cursor.as_ref().map(|cursor| cursor.stream_epoch.clone()),
            },
        );
        let message = ProxyMessage::TerminalAttach {
            session_id: session_id.clone(),
            agent_id: agent.agent_id,
            cols: cols.max(1),
            rows: rows.max(1),
            cursor,
        };
        if let Err(error) = self
            .send_server_frame(workspace_id, &server_id, proxy_message_frame(&message)?)
            .await
        {
            self.inner
                .terminal_sessions
                .lock()
                .await
                .remove(&session_id);
            return Err(error);
        }
        Ok(session_id)
    }

    pub(super) async fn close_agent_terminals(&self, workspace_id: &str, agent_id: &str) {
        let sessions = self
            .inner
            .terminal_sessions
            .lock()
            .await
            .iter()
            .filter(|(_, session)| {
                session.workspace_id == workspace_id && session.process_id == agent_id
            })
            .map(|(session_id, session)| (session_id.clone(), session.outgoing.clone()))
            .collect::<Vec<_>>();
        for (session_id, outgoing) in sessions {
            send_terminal_to_browser(
                &outgoing,
                &TerminalServerMessage::Closed {
                    reason: Some("agent deleted".to_string()),
                    exit_code: None,
                },
            );
            self.detach_terminal(&session_id).await;
        }
    }

    pub async fn terminal_input(
        &self,
        session_id: &str,
        data: Vec<u8>,
    ) -> Result<(), ProtocolError> {
        let (workspace_id, server_id) = self.terminal_session_route(session_id).await?;
        let encoded = TerminalBinaryFrame {
            kind: TerminalBinaryKind::Input,
            session_id: session_id.to_string(),
            revision: 0,
            payload: data,
        }
        .encode()?;
        self.send_server_frame(&workspace_id, &server_id, SocketFrame::Binary(encoded))
            .await
    }

    pub async fn terminal_resize(
        &self,
        session_id: &str,
        cols: u16,
        rows: u16,
    ) -> Result<(), ProtocolError> {
        let (workspace_id, server_id) = self.terminal_session_route(session_id).await?;
        let message = ProxyMessage::TerminalResize {
            session_id: session_id.to_string(),
            cols: cols.max(1),
            rows: rows.max(1),
        };
        self.send_server_frame(&workspace_id, &server_id, proxy_message_frame(&message)?)
            .await
    }

    pub async fn detach_terminal(&self, session_id: &str) {
        let session = self.inner.terminal_sessions.lock().await.remove(session_id);
        let Some(session) = session else {
            return;
        };
        let message = ProxyMessage::TerminalDetach {
            session_id: session_id.to_string(),
        };
        if let Ok(frame) = proxy_message_frame(&message) {
            let _ = self
                .send_server_frame(&session.workspace_id, &session.server_id, frame)
                .await;
        }
    }

    pub async fn terminal_ready(
        &self,
        workspace_id: &str,
        server_id: &str,
        connection_id: Uuid,
        session_id: &str,
        ready: TerminalReadyPayload,
    ) -> Result<(), ProtocolError> {
        self.require_current_connection(workspace_id, server_id, connection_id)
            .await?;
        let replay_chunks = ready.replay.len().div_ceil(TERMINAL_REPLAY_CHUNK_BYTES);
        let replay_chunks = u32::try_from(replay_chunks).map_err(|_| {
            ProtocolError::new(
                "terminal_replay_too_large",
                "terminal replay has too many chunks",
            )
        })?;
        let message = TerminalServerMessage::Ready {
            session_id: session_id.to_string(),
            stream_epoch: ready.stream_epoch.clone(),
            revision: Some(ready.revision),
            gap: ready.gap,
            replay_chunks: Some(replay_chunks),
        };
        self.deliver_session_frame(ClusterSessionDelivery {
            kind: ClusterSessionKind::Terminal,
            workspace_id: workspace_id.to_string(),
            server_id: server_id.to_string(),
            session_id: session_id.to_string(),
            revision: (replay_chunks == 0).then_some(ready.revision),
            cursor: false,
            close: false,
            frame: SocketFrame::Text(
                serde_json::to_string(&message)
                    .map_err(|error| ProtocolError::new("encode_error", error.to_string()))?,
            ),
        })
        .await?;
        for (index, chunk) in ready.replay.chunks(TERMINAL_REPLAY_CHUNK_BYTES).enumerate() {
            let final_chunk = index + 1 == replay_chunks as usize;
            self.deliver_session_frame(ClusterSessionDelivery {
                kind: ClusterSessionKind::Terminal,
                workspace_id: workspace_id.to_string(),
                server_id: server_id.to_string(),
                session_id: session_id.to_string(),
                revision: final_chunk.then_some(ready.revision),
                cursor: false,
                close: false,
                frame: SocketFrame::Binary(chunk.to_vec()),
            })
            .await?;
        }
        Ok(())
    }

    pub async fn terminal_output(
        &self,
        workspace_id: &str,
        server_id: &str,
        connection_id: Uuid,
        session_id: &str,
        revision: u64,
        data: Vec<u8>,
    ) -> Result<(), ProtocolError> {
        self.require_current_connection(workspace_id, server_id, connection_id)
            .await?;
        self.deliver_session_frame(ClusterSessionDelivery {
            kind: ClusterSessionKind::Terminal,
            workspace_id: workspace_id.to_string(),
            server_id: server_id.to_string(),
            session_id: session_id.to_string(),
            revision: Some(revision),
            cursor: true,
            close: false,
            frame: SocketFrame::Binary(data),
        })
        .await
    }

    pub async fn terminal_closed(
        &self,
        workspace_id: &str,
        server_id: &str,
        connection_id: Uuid,
        session_id: &str,
        reason: Option<String>,
        exit_code: Option<i32>,
    ) -> Result<(), ProtocolError> {
        self.require_current_connection(workspace_id, server_id, connection_id)
            .await?;
        self.relay_terminal(
            workspace_id,
            server_id,
            session_id,
            TerminalServerMessage::Closed { reason, exit_code },
            true,
        )
        .await
    }

    pub async fn disconnect_server(
        &self,
        workspace_id: &str,
        server_id: &str,
        connection_id: Uuid,
    ) {
        let key = ServerKey {
            workspace_id: workspace_id.to_string(),
            server_id: server_id.to_string(),
        };
        let removed = {
            let mut connections = self.inner.connections.write().await;
            let is_current = connections
                .get(&key)
                .is_some_and(|connection| connection.connection_id == connection_id);
            is_current
                .then(|| connections.remove(&key))
                .flatten()
                .is_some()
        };
        if !removed {
            return;
        }
        self.inner
            .cluster
            .release(workspace_id, server_id, connection_id)
            .await;
        let payload = serde_json::json!({ "server_id": server_id, "status": "offline" });
        let _ = self
            .mutate_workspace(workspace_id, "server.offline", &payload, |workspace| {
                if let Some(server) = workspace.servers.get_mut(server_id) {
                    server.status = ServerStatus::Offline;
                    server.last_seen_at = Utc::now();
                }
            })
            .await;

        self.close_server_sessions(workspace_id, server_id, "agent server disconnected")
            .await;
    }
}
