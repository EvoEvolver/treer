use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Extension, State, WebSocketUpgrade};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tracing::{debug, warn};
use treer_protocol::{
    AgentServerMessage, ProtocolError, ProxyMessage, TerminalBinaryFrame, TerminalBinaryKind,
    PROTOCOL_VERSION,
};
use uuid::Uuid;

use crate::auth::{AuthStore, MachineSession};
use crate::state::{AppState, SocketFrame};

pub async fn upgrade(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    match auth.authenticate_machine(&headers).await {
        Ok(machine) => ws.on_upgrade(move |socket| handle(socket, state, auth, machine)),
        Err(error) => error.into_response(),
    }
}

async fn handle(socket: WebSocket, state: AppState, auth: AuthStore, machine: MachineSession) {
    let connection_id = Uuid::new_v4();
    let (mut socket_tx, mut socket_rx) = socket.split();
    let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel::<SocketFrame>();
    let writer = tokio::spawn(async move {
        while let Some(frame) = outgoing_rx.recv().await {
            let message = match frame {
                SocketFrame::Text(encoded) => Message::Text(encoded.into()),
                SocketFrame::Binary(encoded) => Message::Binary(encoded.into()),
            };
            if socket_tx.send(message).await.is_err() {
                break;
            }
        }
    });

    let mut identity: Option<(String, String)> = None;
    while let Some(message) = socket_rx.next().await {
        let Ok(message) = message else {
            break;
        };
        let Message::Text(text) = message else {
            match message {
                Message::Binary(encoded) => {
                    let frame = match TerminalBinaryFrame::decode(&encoded) {
                        Ok(frame) => frame,
                        Err(error) => {
                            send_error(&outgoing_tx, error);
                            continue;
                        }
                    };
                    let Some((workspace_id, server_id)) = identity.as_ref() else {
                        send_error(&outgoing_tx, identity_error());
                        continue;
                    };
                    let result = match frame.kind {
                        TerminalBinaryKind::Ready => {
                            state
                                .terminal_ready(
                                    workspace_id,
                                    server_id,
                                    connection_id,
                                    &frame.session_id,
                                    frame.revision,
                                    frame.payload,
                                )
                                .await
                        }
                        TerminalBinaryKind::Output => {
                            state
                                .terminal_output(
                                    workspace_id,
                                    server_id,
                                    connection_id,
                                    &frame.session_id,
                                    frame.revision,
                                    frame.payload,
                                )
                                .await
                        }
                        TerminalBinaryKind::Input => Err(ProtocolError::new(
                            "invalid_terminal_frame",
                            "agent server cannot send terminal input frames",
                        )),
                    };
                    if let Err(error) = result {
                        send_error(&outgoing_tx, error);
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
            continue;
        };
        let parsed = match serde_json::from_str::<AgentServerMessage>(&text) {
            Ok(parsed) => parsed,
            Err(err) => {
                send_error(
                    &outgoing_tx,
                    ProtocolError::new("invalid_message", err.to_string()),
                );
                continue;
            }
        };

        match parsed {
            AgentServerMessage::Register {
                protocol,
                mut server,
            } => {
                if protocol != PROTOCOL_VERSION {
                    send_error(
                        &outgoing_tx,
                        ProtocolError::new(
                            "protocol_mismatch",
                            format!(
                                "agent server uses protocol {protocol}, proxy uses {PROTOCOL_VERSION}"
                            ),
                        ),
                    );
                    break;
                }
                if !machine.allows_server(&server.workspace_id, &server.server_id) {
                    send_error(
                        &outgoing_tx,
                        ProtocolError::new(
                            "machine_identity_mismatch",
                            "registered workspace or server ID does not match machine credentials",
                        ),
                    );
                    break;
                }
                if identity.is_some() {
                    send_error(
                        &outgoing_tx,
                        ProtocolError::new("already_registered", "connection already registered"),
                    );
                    continue;
                }
                let workspace_id = server.workspace_id.clone();
                let server_id = server.server_id.clone();
                if let Err(error) = auth.apply_server_name(&mut server).await {
                    send_error(&outgoing_tx, error.into_parts().1);
                    continue;
                }
                match state
                    .register_server(server, connection_id, outgoing_tx.clone())
                    .await
                {
                    Ok(workspace_revision) => {
                        identity = Some((workspace_id, server_id));
                        let response = ProxyMessage::Registered {
                            protocol: PROTOCOL_VERSION,
                            workspace_revision,
                        };
                        send_message(&outgoing_tx, &response);
                    }
                    Err(error) => send_error(&outgoing_tx, error),
                }
            }
            AgentServerMessage::Snapshot { mut snapshot } => {
                if identity_matches(
                    &identity,
                    &snapshot.server.workspace_id,
                    &snapshot.server.server_id,
                ) {
                    if let Err(error) = auth.apply_server_name(&mut snapshot.server).await {
                        send_error(&outgoing_tx, error.into_parts().1);
                        continue;
                    }
                    let deleted_agents = match auth.apply_agent_names(&mut snapshot).await {
                        Ok(deleted_agents) => deleted_agents,
                        Err(error) => {
                            send_error(&outgoing_tx, error.into_parts().1);
                            continue;
                        }
                    };
                    if let Err(error) = state
                        .restore_deleted_agents(&snapshot.server.workspace_id, deleted_agents)
                        .await
                    {
                        send_error(&outgoing_tx, error);
                        continue;
                    }
                    if let Err(error) = state.apply_snapshot(connection_id, snapshot).await {
                        send_error(&outgoing_tx, error);
                    }
                } else {
                    send_error(&outgoing_tx, identity_error());
                }
            }
            AgentServerMessage::Heartbeat { .. } => {
                if let Some((workspace_id, server_id)) = identity.as_ref() {
                    if let Err(error) = state
                        .heartbeat(workspace_id, server_id, connection_id)
                        .await
                    {
                        send_error(&outgoing_tx, error);
                    }
                } else {
                    send_error(&outgoing_tx, identity_error());
                }
            }
            AgentServerMessage::AgentEvent { agent } => {
                if identity_matches(&identity, &agent.workspace_id, &agent.server_id) {
                    if let Err(error) = state.apply_agent_event(connection_id, agent).await {
                        send_error(&outgoing_tx, error);
                    }
                } else {
                    send_error(&outgoing_tx, identity_error());
                }
            }
            AgentServerMessage::CommandResult { result } => {
                if identity.is_some() {
                    state.complete_command(result).await;
                } else {
                    send_error(&outgoing_tx, identity_error());
                }
            }
            AgentServerMessage::TerminalClosed { session_id, reason } => {
                if let Some((workspace_id, server_id)) = identity.as_ref() {
                    if let Err(error) = state
                        .terminal_closed(
                            workspace_id,
                            server_id,
                            connection_id,
                            &session_id,
                            reason,
                        )
                        .await
                    {
                        send_error(&outgoing_tx, error);
                    }
                } else {
                    send_error(&outgoing_tx, identity_error());
                }
            }
        }
    }

    if let Some((workspace_id, server_id)) = identity {
        state
            .disconnect_server(&workspace_id, &server_id, connection_id)
            .await;
        debug!(%workspace_id, %server_id, "agent server disconnected");
    }
    drop(outgoing_tx);
    let _ = writer.await;
}

fn identity_matches(identity: &Option<(String, String)>, workspace: &str, server: &str) -> bool {
    identity
        .as_ref()
        .is_some_and(|(expected_workspace, expected_server)| {
            expected_workspace == workspace && expected_server == server
        })
}

fn identity_error() -> ProtocolError {
    ProtocolError::new(
        "identity_mismatch",
        "message identity does not match the registered connection",
    )
}

fn send_error(outgoing: &mpsc::UnboundedSender<SocketFrame>, error: ProtocolError) {
    send_message(outgoing, &ProxyMessage::Error { error });
}

fn send_message(outgoing: &mpsc::UnboundedSender<SocketFrame>, message: &ProxyMessage) {
    match serde_json::to_string(message) {
        Ok(encoded) => {
            let _ = outgoing.send(SocketFrame::Text(encoded));
        }
        Err(err) => warn!(%err, "failed to encode proxy message"),
    }
}
