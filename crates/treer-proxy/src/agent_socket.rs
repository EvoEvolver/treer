use std::collections::HashMap;
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Extension, State, WebSocketUpgrade};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;
use tracing::{debug, warn};
use treer_protocol::{
    AgentServerMessage, NetworkBinaryFrame, NetworkBinaryKind, ProtocolError, ProxyMessage,
    TerminalBinaryFrame, TerminalBinaryKind, PROTOCOL_MIN_VERSION, PROTOCOL_VERSION,
};
use uuid::Uuid;

use crate::auth::{AuthStore, MachineSession};
use crate::policy::PolicyEngine;
use crate::state::{AppState, SocketFrame, TerminalReadyPayload};

struct PendingTerminalReady {
    stream_epoch: String,
    gap: bool,
}

const WS_PING_INTERVAL: Duration = Duration::from_secs(20);
const WS_DEAD_INTERVAL: Duration = Duration::from_secs(60);
fn websocket_peer_is_dead(last_activity: Instant, now: Instant) -> bool {
    now.saturating_duration_since(last_activity) >= WS_DEAD_INTERVAL
}

pub async fn upgrade(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    match auth.authenticate_machine(&headers).await {
        Ok(machine) => ws.on_upgrade(move |socket| handle(socket, state, auth, policy, machine)),
        Err(error) => error.into_response(),
    }
}

async fn handle(
    socket: WebSocket,
    state: AppState,
    auth: AuthStore,
    _policy: PolicyEngine,
    machine: MachineSession,
) {
    let connection_id = Uuid::new_v4();
    let (mut socket_tx, mut socket_rx) = socket.split();
    let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel::<SocketFrame>();
    let writer = tokio::spawn(async move {
        while let Some(frame) = outgoing_rx.recv().await {
            let message = match frame {
                SocketFrame::Text(encoded) => Message::Text(encoded.into()),
                SocketFrame::Binary(encoded) => Message::Binary(encoded.into()),
                SocketFrame::Ping(payload) => Message::Ping(payload.into()),
                SocketFrame::Pong(payload) => Message::Pong(payload.into()),
                SocketFrame::Close => Message::Close(None),
            };
            if socket_tx.send(message).await.is_err() {
                break;
            }
        }
    });

    let mut identity: Option<(String, String)> = None;
    let mut pending_terminal_ready: HashMap<String, PendingTerminalReady> = HashMap::new();
    let mut last_activity = Instant::now();
    let mut ping_interval = tokio::time::interval(WS_PING_INTERVAL);
    ping_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    ping_interval.tick().await;
    loop {
        let message = tokio::select! {
            message = socket_rx.next() => {
                let Some(message) = message else {
                    break;
                };
                let Ok(message) = message else {
                    break;
                };
                last_activity = Instant::now();
                message
            }
            _ = ping_interval.tick() => {
                if websocket_peer_is_dead(last_activity, Instant::now()) {
                    debug!("closing silent machine websocket");
                    let _ = outgoing_tx.send(SocketFrame::Close);
                    break;
                }
                let _ = outgoing_tx.send(SocketFrame::Ping(Vec::new()));
                continue;
            }
        };
        let Message::Text(text) = message else {
            match message {
                Message::Binary(encoded) => {
                    if NetworkBinaryFrame::is_network_frame(&encoded) {
                        let frame = match NetworkBinaryFrame::decode(&encoded) {
                            Ok(frame) => frame,
                            Err(error) => {
                                send_error(&outgoing_tx, error);
                                continue;
                            }
                        };
                        let Some((workspace_id, server_id)) = identity.as_ref() else {
                            send_network_reset(&outgoing_tx, &frame.stream_id, identity_error());
                            continue;
                        };
                        let stream_id = frame.stream_id.clone();
                        let result = if matches!(
                            frame.kind,
                            NetworkBinaryKind::Open | NetworkBinaryKind::OpenDatagram
                        ) {
                            send_network_reset(&outgoing_tx, &stream_id, ProtocolError::new(
                                "explicit_service_required", "machine-initiated generic network streams are disabled; use authenticated service forwarding"));
                            continue;
                        } else {
                            state
                                .relay_network_frame(workspace_id, server_id, connection_id, frame)
                                .await
                                .map_err(|error| (stream_id, error))
                        };
                        if let Err((stream_id, error)) = result {
                            send_network_reset(&outgoing_tx, &stream_id, error);
                        }
                        continue;
                    }
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
                            let pending = pending_terminal_ready.remove(&frame.session_id);
                            let (stream_epoch, gap) = pending
                                .map(|pending| (Some(pending.stream_epoch), pending.gap))
                                .unwrap_or((None, false));
                            state
                                .terminal_ready(
                                    workspace_id,
                                    server_id,
                                    connection_id,
                                    &frame.session_id,
                                    TerminalReadyPayload {
                                        revision: frame.revision,
                                        replay: frame.payload,
                                        stream_epoch,
                                        gap,
                                    },
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
                Message::Ping(payload) => {
                    let _ = outgoing_tx.send(SocketFrame::Pong(payload.to_vec()));
                }
                Message::Pong(_) => {}
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
                supported_protocols,
                capabilities,
                controller_instance_id,
                mut server,
            } => {
                let Some(protocol) = negotiate_protocol(protocol, &supported_protocols) else {
                    send_error(
                        &outgoing_tx,
                        ProtocolError::new(
                            "protocol_mismatch",
                            format!(
                                "agent server supports {supported_protocols:?} (legacy {protocol}), proxy supports {PROTOCOL_MIN_VERSION}..={PROTOCOL_VERSION}"
                            ),
                        ),
                    );
                    break;
                };
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
                if !valid_controller_instance_id(&controller_instance_id) {
                    send_error(
                        &outgoing_tx,
                        ProtocolError::new(
                            "invalid_controller_instance",
                            "Controller instance ID is invalid",
                        ),
                    );
                    break;
                }
                let workspace_id = server.workspace_id.clone();
                let server_id = server.server_id.clone();
                if let Err(error) = auth.apply_server_name(&mut server).await {
                    send_error(&outgoing_tx, error.into_parts().1);
                    continue;
                }
                match state
                    .register_server_instance_with_capabilities(
                        server,
                        connection_id,
                        controller_instance_id.clone(),
                        capabilities,
                        outgoing_tx.clone(),
                    )
                    .await
                {
                    Ok(workspace_revision) => {
                        debug!(
                            %workspace_id,
                            %server_id,
                            %controller_instance_id,
                            "agent server registered"
                        );
                        identity = Some((workspace_id.clone(), server_id));
                        let response = ProxyMessage::Registered {
                            protocol,
                            capabilities: Vec::new(),
                            workspace_revision,
                        };
                        send_message(&outgoing_tx, &response);
                        match crate::api::virtual_network_hosts_snapshot(&auth, &workspace_id).await
                        {
                            Ok(snapshot) => send_message(
                                &outgoing_tx,
                                &ProxyMessage::VirtualNetworkHosts { snapshot },
                            ),
                            Err(error) => send_error(&outgoing_tx, error.into_parts().1),
                        }
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
                    let workspace_id = snapshot.server.workspace_id.clone();
                    let server_id = snapshot.server.server_id.clone();
                    if let Err(error) = state.apply_snapshot(connection_id, snapshot).await {
                        send_error(&outgoing_tx, error);
                    } else {
                        tokio::spawn(crate::api::synchronize_app_lifecycles_for_server(
                            state.clone(),
                            auth.clone(),
                            workspace_id,
                            server_id,
                        ));
                    }
                } else {
                    send_error(&outgoing_tx, identity_error());
                }
            }
            AgentServerMessage::Heartbeat { .. } => {
                if let Some((workspace_id, server_id)) = identity.as_ref() {
                    match auth.machine_is_active(workspace_id, server_id).await {
                        Ok(true) => {}
                        Ok(false) => {
                            send_error(
                                &outgoing_tx,
                                ProtocolError::new(
                                    "machine_revoked",
                                    "machine credentials have been revoked",
                                ),
                            );
                            break;
                        }
                        Err(error) => {
                            send_error(&outgoing_tx, error.into_parts().1);
                            continue;
                        }
                    }
                    if let Err(error) = state
                        .heartbeat(workspace_id, server_id, connection_id)
                        .await
                    {
                        let stale = error.code == "stale_connection";
                        send_error(&outgoing_tx, error);
                        if stale {
                            break;
                        }
                    }
                } else {
                    send_error(&outgoing_tx, identity_error());
                }
            }
            AgentServerMessage::AgentEvent { agent } => {
                if identity_matches(&identity, &agent.workspace_id, &agent.server_id) {
                    if let Err(error) = auth.validate_agent_event(&agent).await {
                        send_error(&outgoing_tx, error.into_parts().1);
                        continue;
                    }
                    if let Err(error) = state.apply_agent_event(connection_id, agent).await {
                        send_error(&outgoing_tx, error);
                    }
                } else {
                    send_error(&outgoing_tx, identity_error());
                }
            }
            AgentServerMessage::CommandResult { result } => {
                if let Some((workspace_id, server_id)) = identity.as_ref() {
                    if let Err(error) = state
                        .complete_command_from(workspace_id, server_id, connection_id, result)
                        .await
                    {
                        send_error(&outgoing_tx, error);
                    }
                } else {
                    send_error(&outgoing_tx, identity_error());
                }
            }
            AgentServerMessage::TerminalReady {
                session_id,
                stream_epoch,
                gap,
                ..
            } => {
                pending_terminal_ready
                    .insert(session_id, PendingTerminalReady { stream_epoch, gap });
            }
            AgentServerMessage::TerminalClosed {
                session_id,
                reason,
                exit_code,
            } => {
                if let Some((workspace_id, server_id)) = identity.as_ref() {
                    if let Err(error) = state
                        .terminal_closed(
                            workspace_id,
                            server_id,
                            connection_id,
                            &session_id,
                            reason,
                            exit_code,
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

fn valid_controller_instance_id(value: &str) -> bool {
    value.len() == 36
        && value.starts_with("ctl_")
        && value[4..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn negotiate_protocol(legacy: u32, supported: &[u32]) -> Option<u32> {
    let candidates = if supported.is_empty() {
        std::slice::from_ref(&legacy)
    } else {
        supported
    };
    candidates
        .iter()
        .copied()
        .filter(|version| (PROTOCOL_MIN_VERSION..=PROTOCOL_VERSION).contains(version))
        .max()
}

fn send_network_reset(
    outgoing: &mpsc::UnboundedSender<SocketFrame>,
    stream_id: &str,
    error: ProtocolError,
) {
    let payload = serde_json::to_vec(&error).unwrap_or_default();
    let frame = NetworkBinaryFrame {
        kind: NetworkBinaryKind::Reset,
        stream_id: stream_id.to_string(),
        payload,
    };
    match frame.encode() {
        Ok(encoded) => {
            let _ = outgoing.send(SocketFrame::Binary(encoded));
        }
        Err(error) => send_error(outgoing, error),
    }
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

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn protocol_negotiation_accepts_legacy_and_selects_highest_overlap() {
        assert_eq!(
            negotiate_protocol(PROTOCOL_VERSION, &[]),
            Some(PROTOCOL_VERSION)
        );
        assert_eq!(
            negotiate_protocol(
                PROTOCOL_MIN_VERSION,
                &[PROTOCOL_MIN_VERSION, PROTOCOL_VERSION]
            ),
            Some(PROTOCOL_VERSION)
        );
        assert_eq!(negotiate_protocol(PROTOCOL_VERSION - 1, &[]), None);
        assert_eq!(
            negotiate_protocol(PROTOCOL_VERSION, &[PROTOCOL_VERSION + 1]),
            None
        );
    }

    #[test]
    fn silent_sockets_are_dead_after_the_idle_interval() {
        let now = Instant::now();
        assert!(!websocket_peer_is_dead(now, now));
        assert!(!websocket_peer_is_dead(
            now,
            now + WS_DEAD_INTERVAL - Duration::from_secs(1)
        ));
        assert!(websocket_peer_is_dead(now, now + WS_DEAD_INTERVAL));
    }
}
