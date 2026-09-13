use super::*;
use treer_protocol::AgentStatus;

fn expect_text(frame: SocketFrame) -> String {
    match frame {
        SocketFrame::Text(text) => text,
        SocketFrame::Binary(_) => panic!("expected text socket frame"),
        SocketFrame::Ping(_) | SocketFrame::Pong(_) | SocketFrame::Close => {
            panic!("expected text socket frame")
        }
    }
}

fn expect_network(frame: SocketFrame) -> NetworkBinaryFrame {
    match frame {
        SocketFrame::Binary(encoded) => {
            NetworkBinaryFrame::decode(&encoded).expect("decode network frame")
        }
        SocketFrame::Text(_) => panic!("expected binary network frame"),
        SocketFrame::Ping(_) | SocketFrame::Pong(_) | SocketFrame::Close => {
            panic!("expected binary network frame")
        }
    }
}

fn test_server() -> ServerInfo {
    let now = Utc::now();
    ServerInfo {
        server_id: "server".to_string(),
        workspace_id: "alpha".to_string(),
        name: "test-host".to_string(),
        hostname: "test-host".to_string(),
        root: "/tmp".to_string(),
        controller_build: treer_protocol::BuildInfo {
            version: "0.1.2".to_string(),
            git_commit: "controller-test".to_string(),
        },
        host_build: treer_protocol::BuildInfo {
            version: "0.1.2".to_string(),
            git_commit: "host-test".to_string(),
        },
        supervision: None,
        labels: Default::default(),
        available_agents: None,
        status: ServerStatus::Online,
        connected_at: now,
        last_seen_at: now,
    }
}

#[tokio::test]
async fn workspace_renames_update_snapshots_and_live_events() {
    let state = AppState::new();
    let original = state.ensure_workspace("alpha", "Original").await;
    let mut events = state.subscribe();
    let renamed = WorkspaceInfo {
        name: "Renamed".to_string(),
        ..original
    };

    state
        .rename_workspace_info(renamed.clone())
        .await
        .expect("rename workspace");
    assert_eq!(
        state.snapshot("alpha").await.expect("snapshot").workspace,
        renamed
    );
    let event = events.recv().await.expect("workspace rename event");
    assert_eq!(event.event, "workspace.renamed");
    assert_eq!(event.data["name"], "Renamed");
    state.ensure_workspace("alpha", "alpha").await;
    assert_eq!(
        state
            .snapshot("alpha")
            .await
            .expect("snapshot after runtime ensure")
            .workspace,
        renamed
    );

    let replicated = WorkspaceInfo {
        name: "Replicated".to_string(),
        ..renamed
    };
    state
        .apply_cluster_projection(ClusterProjectionUpdate::WorkspaceUpsert {
            workspace: replicated.clone(),
        })
        .await;
    assert_eq!(
        state.snapshot("alpha").await.expect("snapshot").workspace,
        replicated
    );
    assert_eq!(
        events.recv().await.expect("replicated rename event").event,
        "workspace.renamed"
    );
}

#[tokio::test]
async fn proxy_messages_broadcast_only_to_their_workspace() {
    let state = AppState::new();
    let (alpha_tx, mut alpha_rx) = mpsc::unbounded_channel();
    state
        .register_server(test_server(), Uuid::new_v4(), alpha_tx)
        .await
        .expect("register alpha controller");
    let mut beta = test_server();
    beta.workspace_id = "beta".to_string();
    let (beta_tx, mut beta_rx) = mpsc::unbounded_channel();
    state
        .register_server(beta, Uuid::new_v4(), beta_tx)
        .await
        .expect("register beta controller");
    let message = ProxyMessage::VirtualNetworkHosts {
        snapshot: treer_protocol::VirtualNetworkHostsSnapshot {
            workspace_id: "alpha".to_string(),
            revision: 4,
            hosts: Vec::new(),
        },
    };

    state.broadcast_proxy_message("alpha", &message).await;

    let received: ProxyMessage = serde_json::from_str(&expect_text(
        alpha_rx.recv().await.expect("alpha virtual-host snapshot"),
    ))
    .expect("decode virtual-host snapshot");
    assert_eq!(received, message);
    assert!(beta_rx.try_recv().is_err());
}

fn test_agent(agent_id: &str, name: &str) -> AgentInfo {
    let now = Utc::now();
    AgentInfo {
        agent_id: agent_id.to_string(),
        workspace_id: "alpha".to_string(),
        server_id: "server".to_string(),
        kind: "command".to_string(),
        name: name.to_string(),
        cwd: ".".to_string(),
        status: AgentStatus::Idle,
        pid: None,
        started_at: now,
        updated_at: now,
        exited_at: None,
        exit_code: None,
        output_revision: 0,
        interface: None,
    }
}

#[tokio::test]
async fn platform_agent_count_sums_current_workspace_agents() {
    let state = AppState::new();
    state.ensure_workspace("alpha", "Alpha").await;
    state.ensure_workspace("beta", "Beta").await;
    let mut beta_agent = test_agent("agent-beta", "Beta agent");
    beta_agent.workspace_id = "beta".to_string();
    {
        let mut workspaces = state.inner.workspaces.write().await;
        workspaces
            .get_mut("alpha")
            .expect("alpha workspace")
            .agents
            .insert(
                "agent-alpha".to_string(),
                test_agent("agent-alpha", "Alpha agent"),
            );
        workspaces
            .get_mut("beta")
            .expect("beta workspace")
            .agents
            .insert("agent-beta".to_string(), beta_agent);
    }

    assert_eq!(state.platform_agent_count().await, 2);
}

#[tokio::test]
async fn workspace_snapshots_are_isolated() {
    let state = AppState::new();
    state.ensure_workspace("alpha", "Alpha").await;
    state.ensure_workspace("beta", "Beta").await;

    let alpha = state.snapshot("alpha").await.expect("alpha snapshot");
    let beta = state.snapshot("beta").await.expect("beta snapshot");
    assert_eq!(alpha.workspace.workspace_id, "alpha");
    assert_eq!(beta.workspace.workspace_id, "beta");
    assert!(alpha.servers.is_empty());
    assert!(beta.agents.is_empty());
}

#[tokio::test]
async fn workspace_mutations_publish_versioned_domain_events() {
    let event_bus = EventBus::in_process();
    let mut events = event_bus.subscribe();
    let state = AppState::with_event_bus(event_bus);
    let (outgoing, _incoming) = mpsc::unbounded_channel();

    state
        .register_server(test_server(), Uuid::new_v4(), outgoing)
        .await
        .expect("register server");

    let event = events.recv().await.expect("domain event");
    assert_eq!(event.schema_version, DOMAIN_EVENT_SCHEMA_VERSION);
    assert_eq!(event.workspace_id, "alpha");
    assert_eq!(event.action, "server.updated");
    assert_eq!(event.resource.kind, "workspace");
    assert_eq!(event.resource.id, "alpha");
    assert_eq!(event.workspace_revision, Some(1));
    assert_eq!(event.payload["server_id"], "server");
}

#[tokio::test]
async fn agent_targets_accept_ids_and_unique_names() {
    let state = AppState::new();
    state.ensure_workspace("alpha", "Alpha").await;
    {
        let mut workspaces = state.inner.workspaces.write().await;
        let workspace = workspaces.get_mut("alpha").expect("workspace");
        workspace
            .agents
            .insert("agent-1".to_string(), test_agent("agent-1", "reviewer"));
    }

    assert_eq!(
        state
            .resolve_agent("alpha", "agent-1")
            .await
            .expect("id target")
            .name,
        "reviewer"
    );
    assert_eq!(
        state
            .resolve_agent("alpha", "reviewer")
            .await
            .expect("name target")
            .agent_id,
        "agent-1"
    );
}

#[tokio::test]
async fn duplicate_agent_names_are_ambiguous() {
    let state = AppState::new();
    state.ensure_workspace("alpha", "Alpha").await;
    {
        let mut workspaces = state.inner.workspaces.write().await;
        let workspace = workspaces.get_mut("alpha").expect("workspace");
        workspace
            .agents
            .insert("agent-1".to_string(), test_agent("agent-1", "reviewer"));
        workspace
            .agents
            .insert("agent-2".to_string(), test_agent("agent-2", "reviewer"));
    }

    let error = state
        .resolve_agent("alpha", "reviewer")
        .await
        .expect_err("duplicate names must fail");
    assert_eq!(error.code, "agent_ambiguous");
}

#[tokio::test]
async fn renamed_objects_survive_controller_snapshots_and_events() {
    let state = AppState::new();
    let server = test_server();
    let connection_id = Uuid::new_v4();
    let (outgoing, _messages) = mpsc::unbounded_channel();
    state
        .register_server(server.clone(), connection_id, outgoing)
        .await
        .expect("register server");
    state
        .apply_snapshot(
            connection_id,
            AgentServerSnapshot {
                server: server.clone(),
                agents: vec![test_agent("agent-1", "original-agent")],
            },
        )
        .await
        .expect("initial snapshot");

    state
        .rename_server("alpha", "server", "renamed-machine".to_string())
        .await
        .expect("rename server");
    state
        .rename_agent("alpha", "agent-1", "renamed-agent".to_string())
        .await
        .expect("rename agent");

    state
        .apply_snapshot(
            connection_id,
            AgentServerSnapshot {
                server,
                agents: vec![test_agent("agent-1", "original-agent")],
            },
        )
        .await
        .expect("replacement snapshot");
    state
        .apply_agent_event(connection_id, test_agent("agent-1", "original-agent"))
        .await
        .expect("agent event");

    let snapshot = state.snapshot("alpha").await.expect("workspace snapshot");
    assert_eq!(snapshot.servers[0].name, "renamed-machine");
    assert_eq!(snapshot.agents[0].name, "renamed-agent");
}

#[tokio::test]
async fn deleted_agents_ignore_controller_snapshots_and_events() {
    let state = AppState::new();
    let server = test_server();
    let connection_id = Uuid::new_v4();
    let (outgoing, _messages) = mpsc::unbounded_channel();
    state
        .register_server(server.clone(), connection_id, outgoing)
        .await
        .expect("register server");
    state
        .apply_snapshot(
            connection_id,
            AgentServerSnapshot {
                server: server.clone(),
                agents: vec![test_agent("agent-1", "helper")],
            },
        )
        .await
        .expect("initial snapshot");
    state
        .delete_agent("alpha", "agent-1")
        .await
        .expect("delete agent");

    state
        .apply_snapshot(
            connection_id,
            AgentServerSnapshot {
                server,
                agents: vec![test_agent("agent-1", "helper")],
            },
        )
        .await
        .expect("replacement snapshot");
    state
        .apply_agent_event(connection_id, test_agent("agent-1", "helper"))
        .await
        .expect("late agent event");

    assert!(state
        .snapshot("alpha")
        .await
        .expect("workspace snapshot")
        .agents
        .is_empty());
    assert_eq!(
        state
            .resolve_agent("alpha", "agent-1")
            .await
            .expect_err("deleted agent must stay hidden")
            .code,
        "agent_not_found"
    );
}

#[tokio::test]
async fn deleting_server_closes_resources_and_blocks_late_reconnects() {
    let state = AppState::new();
    let server = test_server();
    let connection_id = Uuid::new_v4();
    let (server_tx, mut server_rx) = mpsc::unbounded_channel();
    state
        .register_server(server.clone(), connection_id, server_tx)
        .await
        .expect("register server");
    state
        .apply_snapshot(
            connection_id,
            AgentServerSnapshot {
                server: server.clone(),
                agents: vec![test_agent("agent-1", "helper")],
            },
        )
        .await
        .expect("apply snapshot");

    let (terminal_tx, mut terminal_rx) = mpsc::channel(TERMINAL_BROWSER_QUEUE_CAPACITY);
    state
        .attach_terminal("alpha", "agent-1", 120, 40, None, terminal_tx)
        .await
        .expect("attach terminal");
    let attach = server_rx.recv().await.expect("terminal attach");
    assert!(matches!(attach, SocketFrame::Text(_)));

    let command_state = state.clone();
    let pending = tokio::spawn(async move {
        command_state
            .send_command(
                "alpha",
                "server",
                AgentCommand::Read {
                    agent_id: "agent-1".to_string(),
                    lines: None,
                },
            )
            .await
    });
    let command = server_rx.recv().await.expect("pending command");
    assert!(matches!(command, SocketFrame::Text(_)));

    let (deleted, agents) = state
        .delete_server("alpha", "server")
        .await
        .expect("delete server");
    assert_eq!(deleted.server_id, "server");
    assert_eq!(agents.len(), 1);
    assert_eq!(server_rx.recv().await, Some(SocketFrame::Close));

    let pending_error = pending
        .await
        .expect("join pending command")
        .expect_err("pending command should fail");
    assert_eq!(pending_error.code, "server_deleted");
    let terminal_message: TerminalServerMessage = serde_json::from_str(&expect_text(
        terminal_rx.recv().await.expect("terminal close"),
    ))
    .expect("decode terminal close");
    assert_eq!(
        terminal_message,
        TerminalServerMessage::Closed {
            reason: Some("machine deleted".to_string()),
            exit_code: None,
        }
    );

    let snapshot = state.snapshot("alpha").await.expect("workspace snapshot");
    assert!(snapshot.servers.is_empty());
    assert!(snapshot.agents.is_empty());
    let (replacement_tx, _replacement_rx) = mpsc::unbounded_channel();
    assert_eq!(
        state
            .register_server(server, Uuid::new_v4(), replacement_tx)
            .await
            .expect_err("deleted server should not reconnect")
            .code,
        "server_deleted"
    );
}

#[tokio::test]
async fn reenrollment_clears_a_deleted_machine_tombstone() {
    let state = AppState::new();
    let server = test_server();
    let connection_id = Uuid::new_v4();
    let (server_tx, _server_rx) = mpsc::unbounded_channel();
    state
        .register_server(server.clone(), connection_id, server_tx)
        .await
        .expect("register machine");
    state
        .delete_server("alpha", "server")
        .await
        .expect("delete machine");

    let (blocked_tx, _blocked_rx) = mpsc::unbounded_channel();
    assert_eq!(
        state
            .register_server(server.clone(), Uuid::new_v4(), blocked_tx)
            .await
            .expect_err("deleted machine remains blocked")
            .code,
        "server_deleted"
    );

    state.allow_server_reenrollment("alpha", "server").await;
    let (reenrolled_tx, _reenrolled_rx) = mpsc::unbounded_channel();
    state
        .register_server(server, Uuid::new_v4(), reenrolled_tx)
        .await
        .expect("reenrolled machine reconnects");
}

#[tokio::test]
async fn pending_command_is_resent_after_controller_snapshot() {
    let state = AppState::new();
    let server = test_server();
    let first_connection = Uuid::new_v4();
    let (first_tx, mut first_rx) = mpsc::unbounded_channel();
    state
        .register_server(server.clone(), first_connection, first_tx)
        .await
        .expect("register first controller");

    let waiting_state = state.clone();
    let waiting = tokio::spawn(async move {
        waiting_state
            .send_command(
                "alpha",
                "server",
                AgentCommand::Read {
                    agent_id: "agent-1".to_string(),
                    lines: None,
                },
            )
            .await
    });
    let first: ProxyMessage = serde_json::from_str(&expect_text(
        first_rx.recv().await.expect("first command should be sent"),
    ))
    .expect("decode first command");
    let ProxyMessage::Command {
        envelope: first_envelope,
    } = first
    else {
        panic!("expected command message");
    };

    state
        .disconnect_server("alpha", "server", first_connection)
        .await;
    let second_connection = Uuid::new_v4();
    let (second_tx, mut second_rx) = mpsc::unbounded_channel();
    state
        .register_server(server.clone(), second_connection, second_tx)
        .await
        .expect("register replacement controller");
    state
        .apply_snapshot(
            second_connection,
            AgentServerSnapshot {
                server,
                agents: Vec::new(),
            },
        )
        .await
        .expect("apply replacement snapshot");

    let second: ProxyMessage = serde_json::from_str(&expect_text(
        second_rx
            .recv()
            .await
            .expect("pending command should be resent"),
    ))
    .expect("decode resent command");
    let ProxyMessage::Command {
        envelope: second_envelope,
    } = second
    else {
        panic!("expected resent command message");
    };
    assert_eq!(first_envelope.command_id, second_envelope.command_id);

    state
        .complete_command(CommandResult::success(
            second_envelope.command_id,
            serde_json::json!({"replayed": true}),
        ))
        .await;
    assert_eq!(
        waiting
            .await
            .expect("join command task")
            .expect("command result"),
        serde_json::json!({"replayed": true})
    );
}

#[tokio::test]
async fn pending_command_is_rejected_when_replacement_lacks_its_capability() {
    let state = AppState::new();
    let server = test_server();
    let first_connection = Uuid::new_v4();
    let (first_tx, mut first_rx) = mpsc::unbounded_channel();
    state
        .register_server(server.clone(), first_connection, first_tx)
        .await
        .expect("register current controller");

    let waiting_state = state.clone();
    let waiting = tokio::spawn(async move {
        waiting_state
            .send_command(
                "alpha",
                "server",
                AgentCommand::Abort {
                    agent_id: "agent-1".to_string(),
                },
            )
            .await
    });
    let _ = first_rx.recv().await.expect("initial command");
    state
        .disconnect_server("alpha", "server", first_connection)
        .await;

    let replacement_connection = Uuid::new_v4();
    let (replacement_tx, mut replacement_rx) = mpsc::unbounded_channel();
    state
        .register_server_instance_with_capabilities(
            server.clone(),
            replacement_connection,
            "ctl_legacy".to_string(),
            Vec::new(),
            replacement_tx,
        )
        .await
        .expect("register legacy replacement");
    state
        .apply_snapshot(
            replacement_connection,
            AgentServerSnapshot {
                server,
                agents: Vec::new(),
            },
        )
        .await
        .expect("apply legacy snapshot");

    let error = waiting
        .await
        .expect("join pending command")
        .expect_err("unsupported pending command must fail");
    assert_eq!(error.code, "unsupported_command");
    assert!(replacement_rx.try_recv().is_err());
}

#[tokio::test]
async fn replacement_controller_fences_the_old_local_connection() {
    let state = AppState::new();
    let server = test_server();
    let first_connection = Uuid::new_v4();
    let (first_tx, mut first_rx) = mpsc::unbounded_channel();
    state
        .register_server_instance(
            server.clone(),
            first_connection,
            "ctl_11111111111111111111111111111111".to_string(),
            first_tx,
        )
        .await
        .expect("register first controller");

    let second_connection = Uuid::new_v4();
    let (second_tx, _second_rx) = mpsc::unbounded_channel();
    state
        .register_server_instance(
            server,
            second_connection,
            "ctl_22222222222222222222222222222222".to_string(),
            second_tx,
        )
        .await
        .expect("register replacement controller");

    let error: ProxyMessage = serde_json::from_str(&expect_text(
        first_rx.recv().await.expect("replacement error"),
    ))
    .expect("decode replacement error");
    assert!(matches!(
        error,
        ProxyMessage::Error { error }
            if error.code == "duplicate_machine_connection"
    ));
    assert_eq!(first_rx.recv().await, Some(SocketFrame::Close));
    assert_eq!(
        state
            .heartbeat("alpha", "server", first_connection)
            .await
            .expect_err("old connection must be fenced")
            .code,
        "duplicate_machine_connection"
    );
    state
        .heartbeat("alpha", "server", second_connection)
        .await
        .expect("replacement owns the machine");
}

#[tokio::test]
async fn original_controller_can_reclaim_after_the_replacement_disconnects() {
    let state = AppState::new();
    let server = test_server();
    let first_connection = Uuid::new_v4();
    let (first_tx, mut first_rx) = mpsc::unbounded_channel();
    state
        .register_server_instance(
            server.clone(),
            first_connection,
            "ctl_11111111111111111111111111111111".to_string(),
            first_tx,
        )
        .await
        .expect("register first controller");

    let second_connection = Uuid::new_v4();
    let (second_tx, _second_rx) = mpsc::unbounded_channel();
    state
        .register_server_instance(
            server.clone(),
            second_connection,
            "ctl_22222222222222222222222222222222".to_string(),
            second_tx,
        )
        .await
        .expect("register replacement controller");
    assert!(first_rx.recv().await.is_some());
    assert_eq!(first_rx.recv().await, Some(SocketFrame::Close));

    state
        .disconnect_server("alpha", "server", second_connection)
        .await;

    let reclaim_connection = Uuid::new_v4();
    let (reclaim_tx, _reclaim_rx) = mpsc::unbounded_channel();
    state
        .register_server_instance(
            server,
            reclaim_connection,
            "ctl_11111111111111111111111111111111".to_string(),
            reclaim_tx,
        )
        .await
        .expect("original controller reclaims the machine");
    state
        .heartbeat("alpha", "server", reclaim_connection)
        .await
        .expect("reclaimed connection owns the machine");
}

#[tokio::test]
async fn machine_shutdown_uses_the_confirmed_command_channel() {
    let state = AppState::new();
    let server = test_server();
    let (server_tx, mut server_rx) = mpsc::unbounded_channel();
    state
        .register_server(server, Uuid::new_v4(), server_tx)
        .await
        .expect("register controller");

    let waiting_state = state.clone();
    let waiting = tokio::spawn(async move {
        waiting_state
            .send_command("alpha", "server", AgentCommand::ShutdownMachine)
            .await
    });
    let message: ProxyMessage = serde_json::from_str(&expect_text(
        server_rx.recv().await.expect("shutdown command"),
    ))
    .expect("decode shutdown command");
    let ProxyMessage::Command { envelope } = message else {
        panic!("expected command message");
    };
    assert_eq!(envelope.command, AgentCommand::ShutdownMachine);

    state
        .complete_command(CommandResult::success(
            envelope.command_id,
            serde_json::json!({"accepted": true}),
        ))
        .await;
    assert_eq!(
        waiting
            .await
            .expect("join shutdown command")
            .expect("shutdown accepted"),
        serde_json::json!({"accepted": true})
    );
}

#[tokio::test]
async fn legacy_controller_rejects_commands_added_after_its_protocol() {
    let state = AppState::new();
    let server = test_server();
    let (server_tx, mut server_rx) = mpsc::unbounded_channel();
    state
        .register_server_instance_with_capabilities(
            server,
            Uuid::new_v4(),
            "ctl_legacy".to_string(),
            Vec::new(),
            server_tx,
        )
        .await
        .expect("register legacy controller");

    let error = state
        .send_command(
            "alpha",
            "server",
            AgentCommand::Abort {
                agent_id: "agent-1".to_string(),
            },
        )
        .await
        .expect_err("legacy controller must not receive unsupported command");
    assert_eq!(error.code, "unsupported_command");
    assert!(server_rx.try_recv().is_err());
}

#[tokio::test]
async fn same_machine_network_streams_use_distinct_leg_ids() {
    let state = AppState::new();
    let connection_id = Uuid::new_v4();
    let (server_tx, mut server_rx) = mpsc::unbounded_channel();
    state
        .register_server(test_server(), connection_id, server_tx)
        .await
        .expect("register controller");

    let source_stream_id = "net_source".to_string();
    let connect_payload = serde_json::to_vec(&NetworkConnectRequest {
        source_server_id: "server".into(),
        source_agent_id: Some("agent-source".into()),
        destination_agent_id: Some("agent-destination".into()),
        host: "127.0.0.1".into(),
        port: 8080,
    })
    .unwrap();
    state
        .open_network_stream(
            "alpha",
            "server",
            connection_id,
            "server",
            NetworkBinaryFrame {
                kind: NetworkBinaryKind::Open,
                stream_id: source_stream_id.clone(),
                payload: connect_payload.clone(),
            },
        )
        .await
        .expect("open same-machine stream");

    let destination_open = expect_network(server_rx.recv().await.expect("destination open frame"));
    assert_eq!(destination_open.kind, NetworkBinaryKind::Open);
    assert_ne!(destination_open.stream_id, source_stream_id);
    assert_eq!(destination_open.payload, connect_payload);
    let destination_stream_id = destination_open.stream_id;
    assert_eq!(state.inner.network_streams.lock().await.len(), 2);

    state
        .relay_network_frame(
            "alpha",
            "server",
            connection_id,
            NetworkBinaryFrame {
                kind: NetworkBinaryKind::Opened,
                stream_id: destination_stream_id.clone(),
                payload: Vec::new(),
            },
        )
        .await
        .expect("relay destination opened frame");
    let source_opened = expect_network(server_rx.recv().await.expect("source opened frame"));
    assert_eq!(source_opened.kind, NetworkBinaryKind::Opened);
    assert_eq!(source_opened.stream_id, source_stream_id);

    state
        .relay_network_frame(
            "alpha",
            "server",
            connection_id,
            NetworkBinaryFrame {
                kind: NetworkBinaryKind::Data,
                stream_id: source_stream_id.clone(),
                payload: b"request".to_vec(),
            },
        )
        .await
        .expect("relay source data");
    let destination_data = expect_network(server_rx.recv().await.expect("destination data frame"));
    assert_eq!(destination_data.stream_id, destination_stream_id);
    assert_eq!(destination_data.payload, b"request");

    state
        .relay_network_frame(
            "alpha",
            "server",
            connection_id,
            NetworkBinaryFrame {
                kind: NetworkBinaryKind::Data,
                stream_id: destination_stream_id.clone(),
                payload: b"response".to_vec(),
            },
        )
        .await
        .expect("relay destination data");
    let source_data = expect_network(server_rx.recv().await.expect("source data frame"));
    assert_eq!(source_data.stream_id, source_stream_id);
    assert_eq!(source_data.payload, b"response");
    let detail = state.recent_agent_traffic("alpha", 1).await.unwrap();
    assert_eq!(detail.len(), 2);
    assert_eq!(
        detail
            .iter()
            .find(|r| r.source_id == "agent-source")
            .unwrap()
            .payload_bytes,
        7
    );
    assert_eq!(
        detail
            .iter()
            .find(|r| r.source_id == "agent-destination")
            .unwrap()
            .payload_bytes,
        8
    );
    assert_eq!(
        state
            .recent_machine_traffic("alpha", 1)
            .await
            .unwrap()
            .iter()
            .map(|r| r.payload_bytes)
            .sum::<u64>(),
        15
    );

    for (stream_id, expected_peer_id) in [
        (source_stream_id.clone(), destination_stream_id.clone()),
        (destination_stream_id, source_stream_id.clone()),
    ] {
        state
            .relay_network_frame(
                "alpha",
                "server",
                connection_id,
                NetworkBinaryFrame {
                    kind: NetworkBinaryKind::HalfClose,
                    stream_id,
                    payload: Vec::new(),
                },
            )
            .await
            .expect("relay half-close");
        let close = expect_network(server_rx.recv().await.expect("peer half-close frame"));
        assert_eq!(close.kind, NetworkBinaryKind::HalfClose);
        assert_eq!(close.stream_id, expected_peer_id);
    }
    assert!(state.inner.network_streams.lock().await.is_empty());
}

#[tokio::test]
async fn relayed_payload_is_counted_once_in_machine_direction() {
    let traffic = TrafficRecorder::default();
    let state = AppState::with_backplanes_and_traffic(
        EventBus::in_process(),
        ClusterBus::standalone("traffic-test".to_string()),
        traffic.clone(),
    );
    let source_connection = Uuid::new_v4();
    let destination_connection = Uuid::new_v4();
    let (source_tx, mut source_rx) = mpsc::unbounded_channel();
    let (destination_tx, mut destination_rx) = mpsc::unbounded_channel();
    let mut source = test_server();
    source.server_id = "source".to_string();
    source.name = "source".to_string();
    let mut destination = test_server();
    destination.server_id = "destination".to_string();
    destination.name = "destination".to_string();
    state
        .register_server(source, source_connection, source_tx)
        .await
        .expect("register source");
    state
        .register_server(destination, destination_connection, destination_tx)
        .await
        .expect("register destination");

    state
        .open_network_stream(
            "alpha",
            "source",
            source_connection,
            "destination",
            NetworkBinaryFrame {
                kind: NetworkBinaryKind::Open,
                stream_id: "source-stream".to_string(),
                payload: b"connect".to_vec(),
            },
        )
        .await
        .expect("open stream");
    let destination_open = expect_network(destination_rx.recv().await.expect("destination open"));

    state
        .relay_network_frame(
            "alpha",
            "source",
            source_connection,
            NetworkBinaryFrame {
                kind: NetworkBinaryKind::Data,
                stream_id: "source-stream".to_string(),
                payload: b"request".to_vec(),
            },
        )
        .await
        .expect("relay request");
    let _ = destination_rx.recv().await.expect("destination data");
    state
        .relay_network_frame(
            "alpha",
            "destination",
            destination_connection,
            NetworkBinaryFrame {
                kind: NetworkBinaryKind::Data,
                stream_id: destination_open.stream_id,
                payload: b"response".to_vec(),
            },
        )
        .await
        .expect("relay response");
    let _ = source_rx.recv().await.expect("source data");

    assert_eq!(
        traffic.pending_for(
            "alpha",
            TrafficClass::VirtualNetwork,
            "source",
            "destination"
        ),
        (7, 1)
    );
    assert_eq!(
        traffic.pending_for(
            "alpha",
            TrafficClass::VirtualNetwork,
            "destination",
            "source"
        ),
        (8, 1)
    );
}

#[tokio::test]
async fn direct_network_routes_do_not_create_proxy_stream_legs() {
    let state = AppState::new();
    let connection_id = Uuid::new_v4();
    let (server_tx, mut server_rx) = mpsc::unbounded_channel();
    state
        .register_server(test_server(), connection_id, server_tx)
        .await
        .expect("register controller");

    let target = NetworkDirectTarget {
        report_usage: false,
        usage_ticket: None,
        host: "example.com".to_string(),
        port: 443,
    };
    state
        .send_direct_network_route(
            "alpha",
            "server",
            connection_id,
            "net_source".to_string(),
            target.clone(),
        )
        .await
        .expect("send direct route");

    let route = expect_network(server_rx.recv().await.expect("direct route frame"));
    assert_eq!(route.kind, NetworkBinaryKind::Direct);
    assert_eq!(route.stream_id, "net_source");
    assert_eq!(
        serde_json::from_slice::<NetworkDirectTarget>(&route.payload)
            .expect("decode direct target"),
        target
    );
    assert!(state.inner.network_streams.lock().await.is_empty());
}

#[tokio::test]
async fn browser_network_streams_bridge_data_and_close_cleanly() {
    let traffic = TrafficRecorder::default();
    let state = AppState::with_backplanes_and_traffic(
        EventBus::in_process(),
        ClusterBus::standalone("browser-traffic-test".to_string()),
        traffic.clone(),
    );
    let connection_id = Uuid::new_v4();
    let (server_tx, mut server_rx) = mpsc::unbounded_channel();
    state
        .register_server(test_server(), connection_id, server_tx)
        .await
        .expect("register controller");

    let opening_state = state.clone();
    let opening = tokio::spawn(async move {
        opening_state
            .open_browser_network_stream(
                "alpha",
                "server",
                Some("agent-a"),
                "127.0.0.1",
                8080,
                TrafficClass::AgentInterface,
            )
            .await
    });
    let open = expect_network(server_rx.recv().await.expect("browser open frame"));
    assert_eq!(open.kind, NetworkBinaryKind::Open);
    let request: NetworkConnectRequest =
        serde_json::from_slice(&open.payload).expect("decode browser connect request");
    assert_eq!(request.host, "127.0.0.1");
    assert_eq!(request.port, 8080);
    assert_eq!(request.source_server_id, "browser");
    assert_eq!(request.destination_agent_id.as_deref(), Some("agent-a"));

    state
        .relay_network_frame(
            "alpha",
            "server",
            connection_id,
            NetworkBinaryFrame {
                kind: NetworkBinaryKind::Opened,
                stream_id: open.stream_id.clone(),
                payload: Vec::new(),
            },
        )
        .await
        .expect("open browser stream");
    let mut browser = opening
        .await
        .expect("join browser open")
        .expect("browser stream");

    browser.write_all(b"request").await.expect("write request");
    let request_data = expect_network(server_rx.recv().await.expect("request data frame"));
    assert_eq!(request_data.kind, NetworkBinaryKind::Data);
    assert_eq!(request_data.stream_id, open.stream_id);
    assert_eq!(request_data.payload, b"request");

    state
        .relay_network_frame(
            "alpha",
            "server",
            connection_id,
            NetworkBinaryFrame {
                kind: NetworkBinaryKind::Data,
                stream_id: open.stream_id.clone(),
                payload: b"response".to_vec(),
            },
        )
        .await
        .expect("relay response");
    let mut response = [0_u8; 8];
    browser
        .read_exact(&mut response)
        .await
        .expect("read response");
    assert_eq!(&response, b"response");
    let window = expect_network(server_rx.recv().await.expect("window update"));
    assert_eq!(window.kind, NetworkBinaryKind::WindowUpdate);

    state
        .relay_network_frame(
            "alpha",
            "server",
            connection_id,
            NetworkBinaryFrame {
                kind: NetworkBinaryKind::HalfClose,
                stream_id: open.stream_id.clone(),
                payload: Vec::new(),
            },
        )
        .await
        .expect("relay remote close");
    browser.shutdown().await.expect("close browser stream");
    let close = expect_network(server_rx.recv().await.expect("browser close frame"));
    assert_eq!(close.kind, NetworkBinaryKind::HalfClose);
    tokio::task::yield_now().await;
    assert!(state.inner.browser_network_streams.lock().await.is_empty());
    assert_eq!(
        traffic.pending_for(
            "alpha",
            TrafficClass::AgentInterface,
            BROWSER_TRAFFIC_ENDPOINT,
            "server"
        ),
        (7, 1)
    );
    assert_eq!(
        traffic.pending_for(
            "alpha",
            TrafficClass::AgentInterface,
            "server",
            BROWSER_TRAFFIC_ENDPOINT
        ),
        (8, 1)
    );
}

#[tokio::test]
async fn cluster_terminal_delivery_learns_epoch_without_local_controller() {
    let state = AppState::new();
    let (browser_tx, mut browser_rx) = mpsc::channel(TERMINAL_BROWSER_QUEUE_CAPACITY);
    state.inner.terminal_sessions.lock().await.insert(
        "remote-session".to_string(),
        TerminalSession {
            workspace_id: "alpha".to_string(),
            server_id: "remote-server".to_string(),
            process_id: "agent".to_string(),
            outgoing: browser_tx,
            last_revision: None,
            stream_epoch: None,
        },
    );
    let ready = TerminalServerMessage::Ready {
        session_id: "remote-session".to_string(),
        stream_epoch: Some("remote-epoch".to_string()),
        revision: Some(4),
        gap: false,
        replay_chunks: Some(0),
    };
    let delivery = ClusterSessionDelivery {
        kind: ClusterSessionKind::Terminal,
        workspace_id: "alpha".to_string(),
        server_id: "remote-server".to_string(),
        session_id: "remote-session".to_string(),
        revision: Some(4),
        cursor: false,
        close: false,
        frame: SocketFrame::Text(serde_json::to_string(&ready).expect("encode ready")),
    };
    state
        .handle_cluster_session_delivery(delivery.clone())
        .await
        .expect("receive ready from Controller's Proxy");
    browser_rx.try_recv().expect("ready reaches browser");
    state
        .handle_cluster_session_delivery(ClusterSessionDelivery {
            revision: Some(5),
            cursor: true,
            frame: SocketFrame::Binary(b"live".to_vec()),
            ..delivery
        })
        .await
        .expect("receive output from Controller's Proxy");
    assert_eq!(
        browser_rx.try_recv().expect("output reaches browser"),
        SocketFrame::Binary(b"live".to_vec())
    );
    let cursor: TerminalServerMessage = serde_json::from_str(&expect_text(
        browser_rx.try_recv().expect("cursor reaches browser"),
    ))
    .expect("decode cursor");
    assert_eq!(
        cursor,
        TerminalServerMessage::Cursor {
            stream_epoch: "remote-epoch".to_string(),
            revision: 5,
        }
    );
}

#[tokio::test]
async fn terminal_routes_raw_binary_and_deduplicates_revisions() {
    let state = AppState::new();
    let server = test_server();
    let connection_id = Uuid::new_v4();
    let (server_tx, mut server_rx) = mpsc::unbounded_channel();
    state
        .register_server(server, connection_id, server_tx)
        .await
        .expect("register controller");
    {
        let mut workspaces = state.inner.workspaces.write().await;
        workspaces
            .get_mut("alpha")
            .expect("workspace")
            .agents
            .insert("agent-1".to_string(), test_agent("agent-1", "shell"));
    }
    let (browser_tx, mut browser_rx) = mpsc::channel(TERMINAL_BROWSER_QUEUE_CAPACITY);
    let session_id = state
        .attach_terminal("alpha", "agent-1", 120, 40, None, browser_tx)
        .await
        .expect("attach terminal");
    let attach: ProxyMessage = serde_json::from_str(&expect_text(
        server_rx.recv().await.expect("terminal attach message"),
    ))
    .expect("decode attach");
    assert!(matches!(
        attach,
        ProxyMessage::TerminalAttach {
            session_id: ref attached,
            cursor: None,
            ..
        } if attached == &session_id
    ));

    let replay = vec![b'x'; TERMINAL_REPLAY_CHUNK_BYTES + 1];
    state
        .terminal_ready(
            "alpha",
            "server",
            connection_id,
            &session_id,
            TerminalReadyPayload {
                revision: 7,
                replay: replay.clone(),
                stream_epoch: Some("stream_a".to_string()),
                gap: false,
            },
        )
        .await
        .expect("terminal ready");
    let ready: TerminalServerMessage =
        serde_json::from_str(&expect_text(browser_rx.recv().await.expect("ready frame")))
            .expect("decode ready");
    assert_eq!(
        ready,
        TerminalServerMessage::Ready {
            session_id: session_id.clone(),
            stream_epoch: Some("stream_a".to_string()),
            revision: Some(7),
            gap: false,
            replay_chunks: Some(2),
        }
    );
    assert_eq!(
        browser_rx.recv().await,
        Some(SocketFrame::Binary(
            replay[..TERMINAL_REPLAY_CHUNK_BYTES].to_vec()
        ))
    );
    assert_eq!(
        browser_rx.recv().await,
        Some(SocketFrame::Binary(
            replay[TERMINAL_REPLAY_CHUNK_BYTES..].to_vec()
        ))
    );

    state
        .terminal_output(
            "alpha",
            "server",
            connection_id,
            &session_id,
            7,
            b"duplicate".to_vec(),
        )
        .await
        .expect("ignore duplicate output");
    assert!(browser_rx.try_recv().is_err());
    state
        .terminal_output(
            "alpha",
            "server",
            connection_id,
            &session_id,
            8,
            b"live".to_vec(),
        )
        .await
        .expect("relay live output");
    assert_eq!(
        browser_rx.recv().await,
        Some(SocketFrame::Binary(b"live".to_vec()))
    );
    let cursor: TerminalServerMessage =
        serde_json::from_str(&expect_text(browser_rx.recv().await.expect("cursor frame")))
            .expect("decode cursor");
    assert_eq!(
        cursor,
        TerminalServerMessage::Cursor {
            stream_epoch: "stream_a".to_string(),
            revision: 8,
        }
    );

    state
        .terminal_input(&session_id, vec![0, 0xff, b'\r'])
        .await
        .expect("relay browser input");
    let input = match server_rx.recv().await.expect("terminal input frame") {
        SocketFrame::Binary(encoded) => {
            TerminalBinaryFrame::decode(&encoded).expect("decode terminal input")
        }
        SocketFrame::Text(_) => panic!("expected binary terminal input"),
        SocketFrame::Ping(_) | SocketFrame::Pong(_) | SocketFrame::Close => {
            panic!("expected binary terminal input")
        }
    };
    assert_eq!(input.kind, TerminalBinaryKind::Input);
    assert_eq!(input.session_id, session_id);
    assert_eq!(input.payload, vec![0, 0xff, b'\r']);
}

#[tokio::test]
async fn slow_terminal_consumer_is_detached_when_its_queue_fills() {
    let state = AppState::new();
    let server = test_server();
    let connection_id = Uuid::new_v4();
    let (server_tx, mut server_rx) = mpsc::unbounded_channel();
    state
        .register_server(server, connection_id, server_tx)
        .await
        .expect("register controller");
    {
        let mut workspaces = state.inner.workspaces.write().await;
        workspaces
            .get_mut("alpha")
            .expect("workspace")
            .agents
            .insert("agent-1".to_string(), test_agent("agent-1", "shell"));
    }
    let (browser_tx, mut browser_rx) = mpsc::channel(1);
    let session_id = state
        .attach_terminal("alpha", "agent-1", 120, 40, None, browser_tx)
        .await
        .expect("attach terminal");
    let _attach = server_rx.recv().await.expect("terminal attach");
    state
        .inner
        .terminal_sessions
        .lock()
        .await
        .get_mut(&session_id)
        .expect("terminal session")
        .stream_epoch = Some("stream_a".to_string());

    state
        .terminal_output(
            "alpha",
            "server",
            connection_id,
            &session_id,
            1,
            b"output".to_vec(),
        )
        .await
        .expect("detach overloaded terminal without blocking controller");

    assert_eq!(
        browser_rx.recv().await,
        Some(SocketFrame::Binary(b"output".to_vec()))
    );
    assert!(!state
        .inner
        .terminal_sessions
        .lock()
        .await
        .contains_key(&session_id));
    let detach: ProxyMessage = serde_json::from_str(&expect_text(
        server_rx.recv().await.expect("terminal detach"),
    ))
    .expect("decode terminal detach");
    assert_eq!(
        detach,
        ProxyMessage::TerminalDetach {
            session_id: session_id.clone(),
        }
    );
}

#[tokio::test]
async fn terminal_attach_forwards_a_stream_cursor() {
    let state = AppState::new();
    let server = test_server();
    let connection_id = Uuid::new_v4();
    let (server_tx, mut server_rx) = mpsc::unbounded_channel();
    state
        .register_server(server, connection_id, server_tx)
        .await
        .expect("register controller");
    {
        let mut workspaces = state.inner.workspaces.write().await;
        workspaces
            .get_mut("alpha")
            .expect("workspace")
            .agents
            .insert("agent-1".to_string(), test_agent("agent-1", "shell"));
    }
    let (browser_tx, _browser_rx) = mpsc::channel(TERMINAL_BROWSER_QUEUE_CAPACITY);
    let cursor = TerminalCursor {
        stream_epoch: "stream_a".to_string(),
        revision: 12,
    };
    state
        .attach_terminal("alpha", "agent-1", 80, 24, Some(cursor.clone()), browser_tx)
        .await
        .expect("attach terminal");
    let attach: ProxyMessage = serde_json::from_str(&expect_text(
        server_rx.recv().await.expect("terminal attach message"),
    ))
    .expect("decode attach");
    match attach {
        ProxyMessage::TerminalAttach {
            cursor: Some(attached),
            ..
        } => assert_eq!(attached, cursor),
        other => panic!("expected attach with cursor, got {other:?}"),
    }
}

#[tokio::test]
async fn cluster_name_projections_survive_snapshot_replay_ordering() {
    let state = AppState::new();
    state.ensure_workspace("alpha", "Alpha").await;
    state
        .apply_cluster_projection(ClusterProjectionUpdate::ServerRenamed {
            workspace_id: "alpha".to_string(),
            server_id: "server".to_string(),
            name: "persisted-server-name".to_string(),
        })
        .await;
    state
        .apply_cluster_projection(ClusterProjectionUpdate::AgentRenamed {
            workspace_id: "alpha".to_string(),
            agent_id: "agent".to_string(),
            name: "persisted-agent-name".to_string(),
        })
        .await;

    let now = Utc::now();
    state
        .apply_cluster_snapshot(ClusterServerSnapshot {
            owner: crate::cluster::ConnectionOwner {
                proxy_id: "remote-proxy".to_string(),
                connection_id: Uuid::new_v4(),
            },
            revision: 1,
            snapshot: AgentServerSnapshot {
                server: test_server(),
                agents: vec![AgentInfo {
                    agent_id: "agent".to_string(),
                    workspace_id: "alpha".to_string(),
                    server_id: "server".to_string(),
                    kind: "command".to_string(),
                    name: "stale-agent-name".to_string(),
                    cwd: ".".to_string(),
                    status: AgentStatus::Idle,
                    pid: None,
                    started_at: now,
                    updated_at: now,
                    exited_at: None,
                    exit_code: None,
                    output_revision: 0,
                    interface: None,
                }],
            },
        })
        .await;

    assert_eq!(
        state
            .resolve_server("alpha", "server")
            .await
            .expect("replayed server")
            .name,
        "persisted-server-name"
    );
    assert_eq!(
        state
            .resolve_agent("alpha", "agent")
            .await
            .expect("replayed agent")
            .name,
        "persisted-agent-name"
    );
}

#[tokio::test]
async fn nats_cluster_routes_projection_commands_terminal_and_network() {
    let Ok(nats_url) = std::env::var("TREER_TEST_NATS_URL") else {
        return;
    };
    let suffix = Uuid::new_v4().simple().to_string();
    let workspace_id = format!("workspace-{suffix}");
    let source_id = format!("source-{suffix}");
    let destination_id = format!("destination-{suffix}");
    let subject_prefix = format!("treer.test.cluster.{suffix}");
    let bus_a = ClusterBus::connect(
        &nats_url,
        format!("proxy-a-{suffix}"),
        subject_prefix.clone(),
    )
    .await
    .expect("connect proxy A cluster bus");
    let bus_b = ClusterBus::connect(&nats_url, format!("proxy-b-{suffix}"), subject_prefix)
        .await
        .expect("connect proxy B cluster bus");
    let state_a = AppState::with_backplanes(EventBus::in_process(), bus_a.clone());
    let state_b = AppState::with_backplanes(EventBus::in_process(), bus_b.clone());
    let workspace = state_a
        .ensure_workspace(&workspace_id, "Cluster test")
        .await;
    bus_a
        .start(state_a.clone())
        .await
        .expect("start proxy A bus");
    bus_a
        .broadcast_projection(ClusterProjectionUpdate::WorkspaceUpsert { workspace })
        .await
        .expect("persist workspace projection before proxy B starts");
    bus_b
        .start(state_b.clone())
        .await
        .expect("start proxy B bus");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if state_b.snapshot(&workspace_id).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("late proxy replays durable workspace projection");

    let now = Utc::now();
    let source_connection = Uuid::new_v4();
    let destination_connection = Uuid::new_v4();
    let (source_tx, mut source_rx) = mpsc::unbounded_channel();
    let source = ServerInfo {
        server_id: source_id.clone(),
        workspace_id: workspace_id.clone(),
        name: "source".to_string(),
        hostname: "source".to_string(),
        root: "/tmp".to_string(),
        controller_build: treer_protocol::BuildInfo {
            version: "0.1.2".to_string(),
            git_commit: "controller-test".to_string(),
        },
        host_build: treer_protocol::BuildInfo {
            version: "0.1.2".to_string(),
            git_commit: "host-test".to_string(),
        },
        supervision: None,
        labels: Default::default(),
        available_agents: None,
        status: ServerStatus::Online,
        connected_at: now,
        last_seen_at: now,
    };
    state_a
        .register_server(source, source_connection, source_tx)
        .await
        .expect("register source on proxy A");

    let (destination_tx, mut destination_rx) = mpsc::unbounded_channel();
    let destination = ServerInfo {
        server_id: destination_id.clone(),
        workspace_id: workspace_id.clone(),
        name: "destination".to_string(),
        hostname: "destination".to_string(),
        root: "/tmp".to_string(),
        controller_build: treer_protocol::BuildInfo {
            version: "0.1.2".to_string(),
            git_commit: "controller-test".to_string(),
        },
        host_build: treer_protocol::BuildInfo {
            version: "0.1.2".to_string(),
            git_commit: "host-test".to_string(),
        },
        supervision: None,
        labels: Default::default(),
        available_agents: None,
        status: ServerStatus::Online,
        connected_at: now,
        last_seen_at: now,
    };
    state_b
        .register_server(destination.clone(), destination_connection, destination_tx)
        .await
        .expect("register destination on proxy B");
    let agent_id = format!("agent-{suffix}");
    state_b
        .apply_snapshot(
            destination_connection,
            AgentServerSnapshot {
                server: destination,
                agents: vec![AgentInfo {
                    agent_id: agent_id.clone(),
                    workspace_id: workspace_id.clone(),
                    server_id: destination_id.clone(),
                    kind: "command".to_string(),
                    name: "remote-agent".to_string(),
                    cwd: ".".to_string(),
                    status: AgentStatus::Idle,
                    pid: None,
                    started_at: now,
                    updated_at: now,
                    exited_at: None,
                    exit_code: None,
                    output_revision: 0,
                    interface: None,
                }],
            },
        )
        .await
        .expect("publish destination snapshot");

    let replicated = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if state_a
                .resolve_agent(&workspace_id, &agent_id)
                .await
                .is_ok()
                && state_b
                    .resolve_server(&workspace_id, &source_id)
                    .await
                    .is_ok()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    if replicated.is_err() {
        panic!(
            "replicate live projections: proxy_a={:?}, proxy_b={:?}",
            state_a.snapshot(&workspace_id).await,
            state_b.snapshot(&workspace_id).await
        );
    }

    state_a
        .rename_agent(&workspace_id, &agent_id, "renamed-remote-agent".to_string())
        .await
        .expect("rename agent through proxy A");
    state_a
        .rename_server(
            &workspace_id,
            &destination_id,
            "renamed-destination".to_string(),
        )
        .await
        .expect("rename server through proxy A");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let agent_name = state_b
                .resolve_agent(&workspace_id, &agent_id)
                .await
                .map(|agent| agent.name);
            let server_name = state_b
                .resolve_server(&workspace_id, &destination_id)
                .await
                .map(|server| server.name);
            if agent_name.as_deref() == Ok("renamed-remote-agent")
                && server_name.as_deref() == Ok("renamed-destination")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("replicate rename projection updates");

    let command_state = state_a.clone();
    let command_workspace = workspace_id.clone();
    let command_server = destination_id.clone();
    let command = tokio::spawn(async move {
        command_state
            .send_command(
                &command_workspace,
                &command_server,
                AgentCommand::ShutdownMachine,
            )
            .await
    });
    let command_message: ProxyMessage = serde_json::from_str(&expect_text(
        destination_rx.recv().await.expect("cross-proxy command"),
    ))
    .expect("decode cross-proxy command");
    let ProxyMessage::Command { envelope } = command_message else {
        panic!("expected command envelope");
    };
    state_b
        .complete_command(CommandResult::success(
            envelope.command_id,
            serde_json::json!({"accepted": true}),
        ))
        .await;
    assert_eq!(
        command
            .await
            .expect("join command")
            .expect("command result"),
        serde_json::json!({"accepted": true})
    );

    let (browser_tx, mut browser_rx) = mpsc::channel(TERMINAL_BROWSER_QUEUE_CAPACITY);
    let session_id = state_a
        .attach_terminal(&workspace_id, &agent_id, 100, 30, None, browser_tx)
        .await
        .expect("attach across proxies");
    let attach: ProxyMessage = serde_json::from_str(&expect_text(
        destination_rx.recv().await.expect("remote terminal attach"),
    ))
    .expect("decode terminal attach");
    assert!(
        matches!(attach, ProxyMessage::TerminalAttach { session_id: attached, .. } if attached == session_id)
    );
    state_b
        .terminal_ready(
            &workspace_id,
            &destination_id,
            destination_connection,
            &session_id,
            TerminalReadyPayload {
                revision: 4,
                replay: b"replay".to_vec(),
                stream_epoch: Some("stream_b".to_string()),
                gap: false,
            },
        )
        .await
        .expect("return terminal replay across proxies");
    assert!(matches!(
        browser_rx.recv().await,
        Some(SocketFrame::Text(_))
    ));
    assert_eq!(
        browser_rx.recv().await,
        Some(SocketFrame::Binary(b"replay".to_vec()))
    );
    state_b
        .terminal_output(
            &workspace_id,
            &destination_id,
            destination_connection,
            &session_id,
            5,
            b"live".to_vec(),
        )
        .await
        .expect("return live terminal output across proxies");
    assert_eq!(
        browser_rx.recv().await,
        Some(SocketFrame::Binary(b"live".to_vec()))
    );
    let cursor: TerminalServerMessage = serde_json::from_str(&expect_text(
        tokio::time::timeout(Duration::from_secs(5), browser_rx.recv())
            .await
            .expect("cross-proxy cursor arrives before the connection lease expires")
            .expect("cross-proxy cursor"),
    ))
    .expect("decode cross-proxy cursor");
    assert_eq!(
        cursor,
        TerminalServerMessage::Cursor {
            stream_epoch: "stream_b".to_string(),
            revision: 5,
        }
    );
    state_a
        .terminal_input(&session_id, b"hello".to_vec())
        .await
        .expect("route terminal input across proxies");
    assert!(matches!(
        destination_rx.recv().await,
        Some(SocketFrame::Binary(_))
    ));

    let source_stream_id = format!("source-stream-{suffix}");
    state_a
        .open_network_stream(
            &workspace_id,
            &source_id,
            source_connection,
            &destination_id,
            NetworkBinaryFrame {
                kind: NetworkBinaryKind::Open,
                stream_id: source_stream_id.clone(),
                payload: b"connect".to_vec(),
            },
        )
        .await
        .expect("open cross-proxy network stream");
    let destination_open = expect_network(destination_rx.recv().await.expect("destination open"));
    assert_eq!(destination_open.kind, NetworkBinaryKind::Open);
    state_b
        .relay_network_frame(
            &workspace_id,
            &destination_id,
            destination_connection,
            NetworkBinaryFrame {
                kind: NetworkBinaryKind::Opened,
                stream_id: destination_open.stream_id.clone(),
                payload: Vec::new(),
            },
        )
        .await
        .expect("return opened frame to coordinating proxy");
    let opened = expect_network(source_rx.recv().await.expect("source opened"));
    assert_eq!(opened.kind, NetworkBinaryKind::Opened);
    assert_eq!(opened.stream_id, source_stream_id);

    for (sender, server_id, connection_id, stream_id, payload) in [
        (
            &state_a,
            &source_id,
            source_connection,
            &source_stream_id,
            b"request\0\xff".to_vec(),
        ),
        (
            &state_b,
            &destination_id,
            destination_connection,
            &destination_open.stream_id,
            b"response\0\xfe".to_vec(),
        ),
    ] {
        sender
            .relay_network_frame(
                &workspace_id,
                server_id,
                connection_id,
                NetworkBinaryFrame {
                    kind: NetworkBinaryKind::Data,
                    stream_id: stream_id.clone(),
                    payload: payload.clone(),
                },
            )
            .await
            .expect("relay binary payload across proxies");
        let received = if server_id == &source_id {
            &mut destination_rx
        } else {
            &mut source_rx
        };
        let frame = expect_network(
            tokio::time::timeout(Duration::from_secs(5), received.recv())
                .await
                .expect("cross-proxy payload arrives")
                .expect("payload frame"),
        );
        assert_eq!(frame.kind, NetworkBinaryKind::Data);
        assert_eq!(frame.payload, payload);
    }
    assert_eq!(
        state_a.inner.traffic.pending_for(
            &workspace_id,
            TrafficClass::VirtualNetwork,
            &source_id,
            &destination_id,
        ),
        (9, 1)
    );
    assert_eq!(
        state_a.inner.traffic.pending_for(
            &workspace_id,
            TrafficClass::VirtualNetwork,
            &destination_id,
            &source_id,
        ),
        (10, 1)
    );

    state_b
        .disconnect_server(&workspace_id, &destination_id, destination_connection)
        .await;
    let terminal_closed = tokio::time::timeout(Duration::from_secs(5), browser_rx.recv())
        .await
        .expect("remote disconnect closes coordinating terminal")
        .expect("terminal close frame");
    let closed: TerminalServerMessage =
        serde_json::from_str(&expect_text(terminal_closed)).expect("decode terminal close");
    assert!(matches!(closed, TerminalServerMessage::Closed { .. }));
    let network_reset = tokio::time::timeout(Duration::from_secs(5), source_rx.recv())
        .await
        .expect("remote disconnect resets coordinating network stream")
        .expect("network reset frame");
    assert_eq!(expect_network(network_reset).kind, NetworkBinaryKind::Reset);
    state_a
        .disconnect_server(&workspace_id, &source_id, source_connection)
        .await;
}
