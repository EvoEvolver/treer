use super::*;

pub(super) async fn workspace_events(
    State(state): State<AppState>,
    Extension(browser): Extension<BrowserAccess>,
    Path(workspace_id): Path<String>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiFailure> {
    browser.validate_if_present(&headers)?;
    state.snapshot(&workspace_id).await?;
    Ok(ws.on_upgrade(move |socket| stream_workspace_events(socket, state, workspace_id)))
}

pub(super) async fn stream_workspace_events(
    socket: WebSocket,
    state: AppState,
    workspace_id: String,
) {
    let (mut outgoing, mut incoming) = socket.split();
    let mut events = state.subscribe();
    if let Ok(snapshot) = state.snapshot(&workspace_id).await {
        let snapshot = visible_workspace_snapshot(snapshot);
        let initial = WorkspaceEvent {
            revision: snapshot.revision,
            workspace_id: workspace_id.clone(),
            event: "workspace.snapshot".to_string(),
            data: serde_json::to_value(snapshot).unwrap_or(Value::Null),
        };
        if send_event(&mut outgoing, &initial).await.is_err() {
            return;
        }
    }

    loop {
        tokio::select! {
            event = events.recv() => match event {
                Ok(event) if event.workspace_id == workspace_id => {
                    if is_internal_app_agent_event(&event) {
                        continue;
                    }
                    if send_event(&mut outgoing, &event).await.is_err() {
                        break;
                    }
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    let Ok(snapshot) = state.snapshot(&workspace_id).await else { break };
                    let snapshot = visible_workspace_snapshot(snapshot);
                    let event = WorkspaceEvent {
                        revision: snapshot.revision,
                        workspace_id: workspace_id.clone(),
                        event: "workspace.snapshot".to_string(),
                        data: serde_json::to_value(snapshot).unwrap_or(Value::Null),
                    };
                    if send_event(&mut outgoing, &event).await.is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
            message = incoming.next() => {
                if message.is_none() || message.is_some_and(|item| item.is_err()) {
                    break;
                }
            }
        }
    }
}

pub(super) async fn send_event(
    outgoing: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    event: &WorkspaceEvent,
) -> Result<(), axum::Error> {
    let encoded = serde_json::to_string(event).unwrap_or_else(|_| "{}".to_string());
    outgoing.send(Message::Text(encoded.into())).await
}
