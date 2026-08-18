use std::net::IpAddr;

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Extension, State, WebSocketUpgrade};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tracing::{debug, warn};
use treer_protocol::{
    AgentServerMessage, NetworkBinaryFrame, NetworkBinaryKind, NetworkConnectRequest,
    NetworkDirectTarget, NetworkOpenRequest, ProtocolError, ProxyMessage, TerminalBinaryFrame,
    TerminalBinaryKind, TransferBinaryFrame, VirtualNetworkHost, PROTOCOL_VERSION,
};
use uuid::Uuid;

use crate::auth::{AuthStore, MachineSession};
use crate::policy::{PolicyEngine, PolicyRequest};
use crate::state::{AppState, SocketFrame};

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
    policy: PolicyEngine,
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
                SocketFrame::Close => Message::Close(None),
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
                        let result = if frame.kind == NetworkBinaryKind::Open {
                            route_network_open(
                                &state,
                                &auth,
                                &policy,
                                workspace_id,
                                server_id,
                                connection_id,
                                frame,
                            )
                            .await
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
                    if TransferBinaryFrame::is_transfer_frame(&encoded) {
                        let frame = match TransferBinaryFrame::decode(&encoded) {
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
                        if let Err(error) = state
                            .transfer_output(
                                workspace_id,
                                server_id,
                                connection_id,
                                frame,
                                encoded.to_vec(),
                            )
                            .await
                        {
                            send_error(&outgoing_tx, error);
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
                        identity = Some((workspace_id.clone(), server_id));
                        let response = ProxyMessage::Registered {
                            protocol: PROTOCOL_VERSION,
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
            AgentServerMessage::TransferReady { session_id } => {
                if let Some((workspace_id, server_id)) = identity.as_ref() {
                    if let Err(error) = state
                        .transfer_ready(workspace_id, server_id, connection_id, &session_id)
                        .await
                    {
                        send_error(&outgoing_tx, error);
                    }
                } else {
                    send_error(&outgoing_tx, identity_error());
                }
            }
            AgentServerMessage::TransferProgress { session_id } => {
                if let Some((workspace_id, server_id)) = identity.as_ref() {
                    if let Err(error) = state
                        .transfer_progress(workspace_id, server_id, connection_id, &session_id)
                        .await
                    {
                        send_error(&outgoing_tx, error);
                    }
                } else {
                    send_error(&outgoing_tx, identity_error());
                }
            }
            AgentServerMessage::TransferComplete { session_id, stats } => {
                if let Some((workspace_id, server_id)) = identity.as_ref() {
                    if let Err(error) = state
                        .transfer_complete(
                            workspace_id,
                            server_id,
                            connection_id,
                            &session_id,
                            stats,
                        )
                        .await
                    {
                        send_error(&outgoing_tx, error);
                    }
                } else {
                    send_error(&outgoing_tx, identity_error());
                }
            }
            AgentServerMessage::TransferFailed { session_id, error } => {
                if let Some((workspace_id, server_id)) = identity.as_ref() {
                    if let Err(error) = state
                        .transfer_failed(workspace_id, server_id, connection_id, &session_id, error)
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

async fn route_network_open(
    state: &AppState,
    auth: &AuthStore,
    policy: &PolicyEngine,
    workspace_id: &str,
    source_server_id: &str,
    connection_id: Uuid,
    mut frame: NetworkBinaryFrame,
) -> Result<(), (String, ProtocolError)> {
    let stream_id = frame.stream_id.clone();
    let request: NetworkOpenRequest = serde_json::from_slice(&frame.payload).map_err(|error| {
        (
            stream_id.clone(),
            ProtocolError::new("invalid_network_open", error.to_string()),
        )
    })?;
    if request.port == 0 || request.host.is_empty() {
        return Err((
            stream_id,
            ProtocolError::new(
                "invalid_network_open",
                "target host and non-zero port are required",
            ),
        ));
    }
    let virtual_host = if request.destination.parse::<IpAddr>().is_ok() {
        None
    } else {
        auth.resolve_virtual_network_host(workspace_id, &request.destination)
            .await
            .map_err(|error| (stream_id.clone(), error.into_parts().1))?
    };
    let route = resolve_network_route(&request, virtual_host);
    let destination_target = route.destination_server_id(source_server_id);
    let destination = state
        .resolve_server(workspace_id, destination_target)
        .await
        .map_err(|error| (stream_id.clone(), error))?;
    if let Some(agent_id) = request.source_agent_id.as_deref() {
        let agent = state
            .resolve_agent(workspace_id, agent_id)
            .await
            .map_err(|error| (stream_id.clone(), error))?;
        if agent.server_id != source_server_id {
            return Err((
                stream_id,
                ProtocolError::new(
                    "policy_subject_mismatch",
                    "network policy subject does not belong to the source machine",
                ),
            ));
        }
    }
    let policy_request = PolicyRequest::network_connect(
        workspace_id,
        source_server_id,
        request.source_agent_id.as_deref(),
        &destination.server_id,
        route.host(),
        route.port(),
    );
    policy
        .authorize(&policy_request)
        .await
        .map_err(|error| (stream_id.clone(), error))?;
    match route {
        ResolvedNetworkRoute::Direct { host, port } => state
            .send_direct_network_route(
                workspace_id,
                source_server_id,
                connection_id,
                stream_id.clone(),
                NetworkDirectTarget { host, port },
            )
            .await
            .map_err(|error| (stream_id, error)),
        ResolvedNetworkRoute::Relay {
            destination_server_id,
            host,
            port,
        } => {
            frame.payload = serde_json::to_vec(&NetworkConnectRequest {
                source_server_id: source_server_id.to_string(),
                source_agent_id: request.source_agent_id,
                host,
                port,
            })
            .map_err(|error| {
                (
                    frame.stream_id.clone(),
                    ProtocolError::new("encode_error", error.to_string()),
                )
            })?;
            state
                .open_network_stream(
                    workspace_id,
                    source_server_id,
                    connection_id,
                    &destination_server_id,
                    frame,
                )
                .await
                .map_err(|error| (stream_id, error))
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ResolvedNetworkRoute {
    Direct {
        host: String,
        port: u16,
    },
    Relay {
        destination_server_id: String,
        host: String,
        port: u16,
    },
}

impl ResolvedNetworkRoute {
    fn destination_server_id<'a>(&'a self, source_server_id: &'a str) -> &'a str {
        match self {
            Self::Direct { .. } => source_server_id,
            Self::Relay {
                destination_server_id,
                ..
            } => destination_server_id,
        }
    }

    fn host(&self) -> &str {
        match self {
            Self::Direct { host, .. } | Self::Relay { host, .. } => host,
        }
    }

    fn port(&self) -> u16 {
        match self {
            Self::Direct { port, .. } | Self::Relay { port, .. } => *port,
        }
    }
}

fn resolve_network_route(
    request: &NetworkOpenRequest,
    virtual_host: Option<VirtualNetworkHost>,
) -> ResolvedNetworkRoute {
    virtual_host.map_or_else(
        || ResolvedNetworkRoute::Direct {
            host: request.host.clone(),
            port: request.port,
        },
        |record| ResolvedNetworkRoute::Relay {
            destination_server_id: record.destination_server_id,
            host: record.target_host,
            port: record.target_port.unwrap_or(request.port),
        },
    )
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
    use chrono::Utc;

    use super::*;

    #[test]
    fn virtual_host_replaces_destination_host_and_optional_port() {
        let request = NetworkOpenRequest {
            destination: "api".to_string(),
            host: "127.0.0.1".to_string(),
            port: 80,
            source_agent_id: None,
        };
        let record = VirtualNetworkHost {
            workspace_id: "default".to_string(),
            hostname: "api".to_string(),
            destination_server_id: "server-b".to_string(),
            target_host: "localhost".to_string(),
            target_port: Some(8080),
            created_at: Utc::now(),
            created_by: "admin".to_string(),
        };
        assert_eq!(
            resolve_network_route(&request, Some(record)),
            ResolvedNetworkRoute::Relay {
                destination_server_id: "server-b".to_string(),
                host: "localhost".to_string(),
                port: 8080,
            }
        );
        assert_eq!(
            resolve_network_route(&request, None),
            ResolvedNetworkRoute::Direct {
                host: "127.0.0.1".to_string(),
                port: 80,
            }
        );
    }
}
