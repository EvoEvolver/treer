use super::*;

impl AppState {
    pub async fn open_browser_network_stream(
        &self,
        workspace_id: &str,
        destination_server_id: &str,
        destination_agent_id: Option<&str>,
        host: &str,
        port: u16,
        traffic_class: TrafficClass,
    ) -> Result<DuplexStream, ProtocolError> {
        let key = NetworkStreamKey {
            workspace_id: workspace_id.to_string(),
            server_id: destination_server_id.to_string(),
            stream_id: self.inner.cluster.routed_id("browser"),
        };
        let (incoming_tx, mut incoming_rx) = mpsc::channel(32);
        self.inner
            .browser_network_streams
            .lock()
            .await
            .insert(key.clone(), incoming_tx);

        let request = NetworkConnectRequest {
            source_server_id: "browser".to_string(),
            source_agent_id: None,
            destination_agent_id: destination_agent_id.map(str::to_string),
            host: host.to_string(),
            port,
        };
        let frame = NetworkBinaryFrame {
            kind: NetworkBinaryKind::Open,
            stream_id: key.stream_id.clone(),
            payload: serde_json::to_vec(&request).map_err(|error| {
                ProtocolError::new(
                    "encode_error",
                    format!("failed to encode network request: {error}"),
                )
            })?,
        };
        if self
            .send_server_frame(
                workspace_id,
                destination_server_id,
                SocketFrame::Binary(frame.encode()?),
            )
            .await
            .is_err()
        {
            self.inner.browser_network_streams.lock().await.remove(&key);
            return Err(ProtocolError::new("server_offline", destination_server_id));
        }

        let opened = tokio::time::timeout(NETWORK_OPEN_TIMEOUT, incoming_rx.recv()).await;
        match opened {
            Ok(Some(frame)) if frame.kind == NetworkBinaryKind::Opened => {}
            Ok(Some(frame)) if frame.kind == NetworkBinaryKind::Reset => {
                self.inner.browser_network_streams.lock().await.remove(&key);
                return Err(decode_network_reset(&frame));
            }
            Ok(Some(frame)) => {
                self.inner.browser_network_streams.lock().await.remove(&key);
                return Err(ProtocolError::new(
                    "invalid_network_frame",
                    format!("expected opened frame, received {:?}", frame.kind),
                ));
            }
            Ok(None) => {
                self.inner.browser_network_streams.lock().await.remove(&key);
                return Err(ProtocolError::new(
                    "network_stream_closed",
                    "network stream closed before it opened",
                ));
            }
            Err(_) => {
                self.reset_browser_network_stream(
                    &key,
                    ProtocolError::new("network_open_timeout", "network connection timed out"),
                )
                .await;
                return Err(ProtocolError::new(
                    "network_open_timeout",
                    "network connection timed out",
                ));
            }
        }

        let (client, bridge) = tokio::io::duplex(NETWORK_INITIAL_WINDOW);
        let traffic = self.inner.traffic.register_client_stream(
            workspace_id,
            traffic_class,
            destination_server_id,
        );
        let state = self.clone();
        tokio::spawn(async move {
            if let Err(error) = state
                .bridge_browser_network_stream(&key, bridge, incoming_rx, traffic)
                .await
            {
                state.reset_browser_network_stream(&key, error).await;
            } else {
                state
                    .inner
                    .browser_network_streams
                    .lock()
                    .await
                    .remove(&key);
            }
        });
        Ok(client)
    }

    pub(super) async fn bridge_browser_network_stream(
        &self,
        key: &NetworkStreamKey,
        stream: DuplexStream,
        mut incoming: mpsc::Receiver<NetworkBinaryFrame>,
        traffic: StreamTrafficCounters,
    ) -> Result<(), ProtocolError> {
        let (mut reader, mut writer) = tokio::io::split(stream);
        let mut buffer = vec![0_u8; NETWORK_MAX_CHUNK];
        let mut send_window = NETWORK_INITIAL_WINDOW;
        let mut local_closed = false;
        let mut remote_closed = false;
        while !local_closed || !remote_closed {
            tokio::select! {
                read = reader.read(&mut buffer[..send_window.min(NETWORK_MAX_CHUNK)]), if !local_closed && send_window > 0 => {
                    let read = read.map_err(|error| ProtocolError::new("network_io_error", error.to_string()))?;
                    if read == 0 {
                        local_closed = true;
                        self.send_browser_network_frame(key, NetworkBinaryKind::HalfClose, Vec::new()).await?;
                    } else {
                        send_window -= read;
                        self.send_browser_network_frame(key, NetworkBinaryKind::Data, buffer[..read].to_vec()).await?;
                        traffic.source_to_destination.record(read);
                    }
                }
                frame = incoming.recv() => {
                    let frame = frame.ok_or_else(|| ProtocolError::new("network_stream_closed", "network stream receiver closed"))?;
                    match frame.kind {
                        NetworkBinaryKind::Data => {
                            writer.write_all(&frame.payload).await.map_err(|error| ProtocolError::new("network_io_error", error.to_string()))?;
                            traffic.destination_to_source.record(frame.payload.len());
                            let amount = u32::try_from(frame.payload.len()).unwrap_or(u32::MAX).to_be_bytes().to_vec();
                            self.send_browser_network_frame(key, NetworkBinaryKind::WindowUpdate, amount).await?;
                        }
                        NetworkBinaryKind::WindowUpdate => {
                            let bytes: [u8; 4] = frame.payload.as_slice().try_into().map_err(|_| ProtocolError::new("invalid_network_frame", "invalid network window update"))?;
                            send_window = send_window.saturating_add(u32::from_be_bytes(bytes) as usize);
                        }
                        NetworkBinaryKind::HalfClose => {
                            if !remote_closed {
                                writer.shutdown().await.map_err(|error| ProtocolError::new("network_io_error", error.to_string()))?;
                                remote_closed = true;
                            }
                        }
                        NetworkBinaryKind::Reset => return Err(decode_network_reset(&frame)),
                        NetworkBinaryKind::Open | NetworkBinaryKind::OpenDatagram
                        | NetworkBinaryKind::Opened
                        | NetworkBinaryKind::Direct
                        | NetworkBinaryKind::Usage | NetworkBinaryKind::UsageAck => {
                            return Err(ProtocolError::new("invalid_network_frame", format!("unexpected network stream frame {:?}", frame.kind)));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    pub(super) async fn send_browser_network_frame(
        &self,
        key: &NetworkStreamKey,
        kind: NetworkBinaryKind,
        payload: Vec<u8>,
    ) -> Result<(), ProtocolError> {
        let frame = NetworkBinaryFrame {
            kind,
            stream_id: key.stream_id.clone(),
            payload,
        };
        self.send_server_frame(
            &key.workspace_id,
            &key.server_id,
            SocketFrame::Binary(frame.encode()?),
        )
        .await
    }

    pub(super) async fn reset_browser_network_stream(
        &self,
        key: &NetworkStreamKey,
        error: ProtocolError,
    ) {
        self.inner.browser_network_streams.lock().await.remove(key);
        let payload = serde_json::to_vec(&error).unwrap_or_default();
        let _ = self
            .send_browser_network_frame(key, NetworkBinaryKind::Reset, payload)
            .await;
    }

    pub async fn open_network_stream(
        &self,
        workspace_id: &str,
        source_server_id: &str,
        connection_id: Uuid,
        destination_server_id: &str,
        mut frame: NetworkBinaryFrame,
    ) -> Result<(), ProtocolError> {
        self.require_current_connection(workspace_id, source_server_id, connection_id)
            .await?;
        if !matches!(
            frame.kind,
            NetworkBinaryKind::Open | NetworkBinaryKind::OpenDatagram
        ) {
            return Err(ProtocolError::new(
                "invalid_network_frame",
                "new network stream must begin with an open frame",
            ));
        }
        let source = NetworkStreamKey {
            workspace_id: workspace_id.to_string(),
            server_id: source_server_id.to_string(),
            stream_id: frame.stream_id.clone(),
        };
        let destination = NetworkStreamKey {
            workspace_id: workspace_id.to_string(),
            server_id: destination_server_id.to_string(),
            stream_id: self.inner.cluster.routed_id("net"),
        };
        let traffic = self.inner.traffic.register_machine_stream(
            workspace_id,
            source_server_id,
            destination_server_id,
        );
        let agent_traffic = serde_json::from_slice::<NetworkConnectRequest>(&frame.payload)
            .ok()
            .and_then(|request| {
                self.inner.traffic.agent_view().register_agent_stream(
                    workspace_id,
                    source_server_id,
                    destination_server_id,
                    request.source_agent_id.as_deref(),
                    request.destination_agent_id.as_deref(),
                )
            });
        {
            let mut streams = self.inner.network_streams.lock().await;
            if streams.contains_key(&source) || streams.contains_key(&destination) {
                return Err(ProtocolError::new(
                    "network_stream_exists",
                    "network stream ID is already in use",
                ));
            }
            streams.insert(
                source.clone(),
                NetworkStreamLeg {
                    peer: destination.clone(),
                    role: NetworkStreamRole::Source,
                    closed: false,
                    outgoing_traffic: traffic.source_to_destination,
                    outgoing_agent_traffic: agent_traffic
                        .as_ref()
                        .map(|c| c.source_to_destination.clone()),
                },
            );
            streams.insert(
                destination.clone(),
                NetworkStreamLeg {
                    peer: source.clone(),
                    role: NetworkStreamRole::Destination,
                    closed: false,
                    outgoing_traffic: traffic.destination_to_source,
                    outgoing_agent_traffic: agent_traffic
                        .as_ref()
                        .map(|c| c.destination_to_source.clone()),
                },
            );
        }
        frame.stream_id.clone_from(&destination.stream_id);
        let encoded = frame.encode()?;
        if self
            .send_server_frame(
                workspace_id,
                destination_server_id,
                SocketFrame::Binary(encoded),
            )
            .await
            .is_err()
        {
            let mut streams = self.inner.network_streams.lock().await;
            remove_network_stream(&mut streams, &source);
            return Err(ProtocolError::new("server_offline", destination_server_id));
        }
        Ok(())
    }

    pub async fn send_direct_network_route(
        &self,
        workspace_id: &str,
        source_server_id: &str,
        connection_id: Uuid,
        stream_id: String,
        target: NetworkDirectTarget,
    ) -> Result<(), ProtocolError> {
        let key = ServerKey {
            workspace_id: workspace_id.to_string(),
            server_id: source_server_id.to_string(),
        };
        let outgoing = {
            let connections = self.inner.connections.read().await;
            match connections.get(&key) {
                Some(connection) if connection.connection_id == connection_id => {
                    connection.outgoing.clone()
                }
                _ => {
                    return Err(ProtocolError::new(
                        "stale_connection",
                        format!("connection for {source_server_id} is no longer current"),
                    ));
                }
            }
        };
        let frame = NetworkBinaryFrame {
            kind: NetworkBinaryKind::Direct,
            stream_id,
            payload: serde_json::to_vec(&target).map_err(|error| {
                ProtocolError::new(
                    "encode_error",
                    format!("failed to encode direct route: {error}"),
                )
            })?,
        };
        outgoing
            .send(SocketFrame::Binary(frame.encode()?))
            .map_err(|_| ProtocolError::new("server_offline", source_server_id))
    }

    pub async fn relay_network_frame(
        &self,
        workspace_id: &str,
        server_id: &str,
        connection_id: Uuid,
        frame: NetworkBinaryFrame,
    ) -> Result<(), ProtocolError> {
        self.require_current_connection(workspace_id, server_id, connection_id)
            .await?;
        self.relay_network_frame_inner(workspace_id, server_id, frame)
            .await
    }

    pub(crate) async fn has_network_stream(
        &self,
        workspace_id: &str,
        server_id: &str,
        stream_id: &str,
    ) -> bool {
        self.inner
            .network_streams
            .lock()
            .await
            .contains_key(&NetworkStreamKey {
                workspace_id: workspace_id.to_string(),
                server_id: server_id.to_string(),
                stream_id: stream_id.to_string(),
            })
    }

    pub(super) async fn relay_network_frame_inner(
        &self,
        workspace_id: &str,
        server_id: &str,
        mut frame: NetworkBinaryFrame,
    ) -> Result<(), ProtocolError> {
        if matches!(
            frame.kind,
            NetworkBinaryKind::Open
                | NetworkBinaryKind::OpenDatagram
                | NetworkBinaryKind::Direct
                | NetworkBinaryKind::Usage
                | NetworkBinaryKind::UsageAck
        ) {
            return Err(ProtocolError::new(
                "invalid_network_frame",
                "route frame cannot be relayed as an existing stream",
            ));
        }
        let key = NetworkStreamKey {
            workspace_id: workspace_id.to_string(),
            server_id: server_id.to_string(),
            stream_id: frame.stream_id.clone(),
        };
        let browser = self
            .inner
            .browser_network_streams
            .lock()
            .await
            .get(&key)
            .cloned();
        if let Some(browser) = browser {
            if browser.send(frame).await.is_err() {
                self.inner.browser_network_streams.lock().await.remove(&key);
                return Err(ProtocolError::new(
                    "network_stream_closed",
                    "browser network stream closed",
                ));
            }
            return Ok(());
        }
        let route = {
            let mut streams = self.inner.network_streams.lock().await;
            let Some(stream) = streams.get_mut(&key) else {
                drop(streams);
                let target = ClusterBus::route_target(&frame.stream_id).ok_or_else(|| {
                    ProtocolError::new("network_stream_not_found", &frame.stream_id)
                })?;
                if target == self.inner.cluster.instance_id() {
                    return Err(ProtocolError::new(
                        "network_stream_not_found",
                        &frame.stream_id,
                    ));
                }
                return self
                    .inner
                    .cluster
                    .deliver_network(&target, workspace_id, server_id, frame.encode()?)
                    .await;
            };
            if frame.kind == NetworkBinaryKind::Opened
                && stream.role != NetworkStreamRole::Destination
            {
                return Err(ProtocolError::new(
                    "invalid_network_frame",
                    "only the destination can open a network stream",
                ));
            }
            if frame.kind == NetworkBinaryKind::HalfClose {
                stream.closed = true;
            }
            let peer = stream.peer.clone();
            let traffic =
                (frame.kind == NetworkBinaryKind::Data).then(|| stream.outgoing_traffic.clone());
            let agent_traffic = (frame.kind == NetworkBinaryKind::Data)
                .then(|| stream.outgoing_agent_traffic.clone())
                .flatten();
            let remove = frame.kind == NetworkBinaryKind::Reset
                || (stream.closed && streams.get(&peer).is_some_and(|peer| peer.closed));
            if remove {
                remove_network_stream(&mut streams, &key);
            }
            (peer, remove, traffic, agent_traffic)
        };
        let (peer, remove, traffic, agent_traffic) = route;
        let payload_bytes = frame.payload.len();
        frame.stream_id.clone_from(&peer.stream_id);
        if self
            .send_server_frame(
                workspace_id,
                &peer.server_id,
                SocketFrame::Binary(frame.encode()?),
            )
            .await
            .is_err()
            && !remove
        {
            let mut streams = self.inner.network_streams.lock().await;
            remove_network_stream(&mut streams, &key);
            return Err(ProtocolError::new("server_offline", peer.server_id));
        }
        if let Some(traffic) = traffic {
            traffic.record(payload_bytes);
        }
        if let Some(traffic) = agent_traffic {
            traffic.record(payload_bytes);
        }
        Ok(())
    }

    pub(crate) async fn handle_cluster_network_delivery(
        &self,
        workspace_id: &str,
        server_id: &str,
        encoded: Vec<u8>,
    ) -> Result<(), ProtocolError> {
        let frame = NetworkBinaryFrame::decode(&encoded)?;
        self.relay_network_frame_inner(workspace_id, server_id, frame)
            .await
    }

    pub(super) async fn close_server_network_streams(&self, workspace_id: &str, server_id: &str) {
        let browser_streams = {
            let mut streams = self.inner.browser_network_streams.lock().await;
            let keys = streams
                .keys()
                .filter(|key| key.workspace_id == workspace_id && key.server_id == server_id)
                .cloned()
                .collect::<Vec<_>>();
            keys.into_iter()
                .filter_map(|key| streams.remove(&key).map(|sender| (key, sender)))
                .collect::<Vec<_>>()
        };
        let browser_payload = serde_json::to_vec(&ProtocolError::new(
            "server_offline",
            "agent server disconnected",
        ))
        .unwrap_or_default();
        for (key, sender) in browser_streams {
            let _ = sender
                .send(NetworkBinaryFrame {
                    kind: NetworkBinaryKind::Reset,
                    stream_id: key.stream_id,
                    payload: browser_payload.clone(),
                })
                .await;
        }

        let routes = {
            let mut streams = self.inner.network_streams.lock().await;
            let keys = streams
                .keys()
                .filter(|key| key.workspace_id == workspace_id && key.server_id == server_id)
                .cloned()
                .collect::<Vec<_>>();
            let mut routes = Vec::new();
            for key in keys {
                let Some(stream) = remove_network_stream(&mut streams, &key) else {
                    continue;
                };
                if stream.peer.server_id != server_id {
                    routes.push(stream.peer);
                }
            }
            routes
        };
        let payload = serde_json::to_vec(&ProtocolError::new(
            "server_offline",
            "network peer disconnected",
        ))
        .unwrap_or_default();
        for peer in routes {
            let frame = NetworkBinaryFrame {
                kind: NetworkBinaryKind::Reset,
                stream_id: peer.stream_id,
                payload: payload.clone(),
            };
            if let Ok(encoded) = frame.encode() {
                let _ = self
                    .send_server_frame(workspace_id, &peer.server_id, SocketFrame::Binary(encoded))
                    .await;
            }
        }
    }

    pub(super) async fn close_server_sessions(
        &self,
        workspace_id: &str,
        server_id: &str,
        reason: &str,
    ) {
        let disconnected = {
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
        for session in disconnected {
            send_terminal_to_browser(
                &session.outgoing,
                &TerminalServerMessage::Closed {
                    reason: Some(reason.to_string()),
                    exit_code: None,
                },
            );
        }
        self.close_server_network_streams(workspace_id, server_id)
            .await;
    }
}
