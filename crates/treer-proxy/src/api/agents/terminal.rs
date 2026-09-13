use super::*;
#[derive(Debug, Deserialize)]
pub(crate) struct TerminalQuery {
    #[serde(default = "default_terminal_cols")]
    cols: u16,
    #[serde(default = "default_terminal_rows")]
    rows: u16,
    #[serde(default)]
    stream_epoch: Option<String>,
    #[serde(default)]
    since_revision: Option<u64>,
    #[serde(default)]
    flow_control: bool,
}

const fn default_terminal_cols() -> u16 {
    120
}

const fn default_terminal_rows() -> u16 {
    36
}

impl TerminalQuery {
    fn cursor(&self) -> Option<TerminalCursor> {
        let stream_epoch = self.stream_epoch.as_deref()?.trim();
        if stream_epoch.is_empty() {
            return None;
        }
        Some(TerminalCursor {
            stream_epoch: stream_epoch.to_string(),
            revision: self.since_revision.unwrap_or(0),
        })
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn agent_terminal(
    State(state): State<AppState>,
    Extension(browser): Extension<BrowserAccess>,
    Extension(policy): Extension<PolicyEngine>,
    machine: Option<Extension<MachineSession>>,
    Path((workspace_id, agent_id)): Path<(String, String)>,
    Query(query): Query<TerminalQuery>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiFailure> {
    browser.validate_if_present(&headers)?;
    let agent = state.resolve_agent(&workspace_id, &agent_id).await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    require_machine_target(subject.as_ref(), &agent.server_id)?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_AGENT_INPUT,
        agent_policy_resource(&agent),
    )
    .await?;
    Ok(ws.on_upgrade(move |socket| stream_terminal(socket, state, workspace_id, agent_id, query)))
}

pub(crate) async fn stream_terminal(
    socket: WebSocket,
    state: AppState,
    workspace_id: String,
    agent_id: String,
    query: TerminalQuery,
) {
    let (mut outgoing, mut incoming) = socket.split();
    let (terminal_tx, mut terminal_rx) =
        tokio::sync::mpsc::channel::<SocketFrame>(TERMINAL_BROWSER_QUEUE_CAPACITY);
    let cursor = query.cursor();
    let attached = state
        .attach_terminal(
            &workspace_id,
            &agent_id,
            query.cols,
            query.rows,
            cursor,
            terminal_tx,
        )
        .await;
    let session_id = match attached {
        Ok(session_id) => session_id,
        Err(error) => {
            let message = TerminalServerMessage::Error { error };
            if let Ok(encoded) = serde_json::to_string(&message) {
                let _ = outgoing.send(Message::Text(encoded.into())).await;
            }
            return;
        }
    };

    let flow_window_bytes = query.flow_control.then_some(TERMINAL_FLOW_WINDOW_BYTES);
    let mut in_flight_bytes = 0usize;
    loop {
        tokio::select! {
            message = incoming.next() => {
                let Some(Ok(message)) = message else { break };
                let result = match message {
                    Message::Binary(data) => state.terminal_input(&session_id, data.to_vec()).await,
                    Message::Text(text) => match serde_json::from_str::<TerminalClientMessage>(&text) {
                        Ok(TerminalClientMessage::Resize { cols, rows }) => {
                            state.terminal_resize(&session_id, cols, rows).await
                        }
                        Ok(TerminalClientMessage::Ack { bytes }) => {
                            let Some(_) = flow_window_bytes else {
                                continue;
                            };
                            let bytes = bytes as usize;
                            if bytes > in_flight_bytes {
                                Err(ProtocolError::new(
                                    "invalid_terminal_ack",
                                    "terminal acknowledgement exceeds outstanding output",
                                ))
                            } else {
                                in_flight_bytes -= bytes;
                                Ok(())
                            }
                        }
                        Err(error) => Err(ProtocolError::new("invalid_terminal_message", error.to_string())),
                    },
                    Message::Close(_) => break,
                    _ => continue,
                };
                if let Err(error) = result {
                    let message = TerminalServerMessage::Error { error };
                    if let Ok(encoded) = serde_json::to_string(&message) {
                        if outgoing.send(Message::Text(encoded.into())).await.is_err() {
                            break;
                        }
                    }
                }
            }
            frame = terminal_rx.recv(), if flow_window_bytes.is_none_or(|window| in_flight_bytes < window) => {
                let Some(frame) = frame else { break };
                let binary_bytes = match &frame {
                    SocketFrame::Binary(data) => data.len(),
                    _ => 0,
                };
                let message = match frame {
                    SocketFrame::Text(encoded) => Message::Text(encoded.into()),
                    SocketFrame::Binary(data) => Message::Binary(data.into()),
                    SocketFrame::Ping(payload) => Message::Ping(payload.into()),
                    SocketFrame::Pong(payload) => Message::Pong(payload.into()),
                    SocketFrame::Close => Message::Close(None),
                };
                if outgoing.send(message).await.is_err() {
                    break;
                }
                if flow_window_bytes.is_some() {
                    in_flight_bytes = in_flight_bytes.saturating_add(binary_bytes);
                }
            }
        }
    }
    state.detach_terminal(&session_id).await;
}
