use super::*;
use treer_protocol::AgentStatus;

fn expect_text(frame: SocketFrame) -> String {
    match frame {
        SocketFrame::Text(text) => text,
        other => panic!("expected text socket frame, got {other:?}"),
    }
}

fn expect_network(frame: SocketFrame) -> NetworkBinaryFrame {
    match frame {
        SocketFrame::Binary(encoded) => {
            NetworkBinaryFrame::decode(&encoded).expect("decode network frame")
        }
        other => panic!("expected binary network frame, got {other:?}"),
    }
}

fn expect_envelope(frame: SocketFrame) -> treer_protocol::CommandEnvelope {
    match serde_json::from_str(&expect_text(frame)).unwrap() {
        ProxyMessage::Command { envelope } => envelope,
        other => panic!("expected command, got {other:?}"),
    }
}

fn test_server() -> ServerInfo {
    server_in("alpha", "server")
}

fn server_in(workspace_id: &str, server_id: &str) -> ServerInfo {
    let now = Utc::now();
    let build = treer_protocol::BuildInfo {
        version: "0.1.2".to_string(),
        git_commit: "test".to_string(),
    };
    ServerInfo {
        server_id: server_id.to_string(),
        workspace_id: workspace_id.to_string(),
        name: server_id.to_string(),
        hostname: "test-host".to_string(),
        root: "/tmp".to_string(),
        controller_build: build.clone(),
        host_build: build,
        supervision: None,
        labels: Default::default(),
        available_agents: None,
        status: ServerStatus::Online,
        connected_at: now,
        last_seen_at: now,
        capabilities: vec![],
    }
}

fn test_agent(agent_id: &str, name: &str) -> AgentInfo {
    agent_in("alpha", "server", agent_id, name)
}

fn agent_in(workspace_id: &str, server_id: &str, agent_id: &str, name: &str) -> AgentInfo {
    let now = Utc::now();
    AgentInfo {
        path: None,
        run_generation: 0,
        agent_id: agent_id.to_string(),
        workspace_id: workspace_id.to_string(),
        server_id: server_id.to_string(),
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

fn snapshot_of(server: ServerInfo, agents: Vec<AgentInfo>) -> AgentServerSnapshot {
    AgentServerSnapshot { server, agents }
}

/// Registers the standard `alpha/server` Controller with explicit capabilities.
async fn connect(
    state: &AppState,
    capabilities: &[&str],
) -> (Uuid, mpsc::UnboundedReceiver<SocketFrame>) {
    let connection = Uuid::new_v4();
    let (tx, rx) = mpsc::unbounded_channel();
    state
        .register_server_instance_with_capabilities(
            test_server(),
            connection,
            "controller".into(),
            capabilities.iter().map(|c| c.to_string()),
            tx,
        )
        .await
        .unwrap();
    (connection, rx)
}

/// Registers `alpha/server` with the full current capability set.
async fn connect_current(state: &AppState) -> (Uuid, mpsc::UnboundedReceiver<SocketFrame>) {
    let connection = Uuid::new_v4();
    let (tx, rx) = mpsc::unbounded_channel();
    state
        .register_server(test_server(), connection, tx)
        .await
        .unwrap();
    (connection, rx)
}

fn spawn_command(
    state: &AppState,
    command: AgentCommand,
) -> tokio::task::JoinHandle<Result<serde_json::Value, ProtocolError>> {
    let state = state.clone();
    tokio::spawn(async move { state.send_command("alpha", "server", command).await })
}

async fn insert_agent(state: &AppState, agent: AgentInfo) {
    state
        .inner
        .workspaces
        .write()
        .await
        .get_mut(&agent.workspace_id)
        .expect("workspace")
        .agents
        .insert(agent.agent_id.clone(), agent);
}

fn read_cmd() -> AgentCommand {
    AgentCommand::Read {
        agent_id: "agent-1".to_string(),
        lines: None,
    }
}

#[tokio::test]
async fn durable_creation_guard_rejects_deleted_dispatch_forward_and_replay() {
    let auth = crate::auth::AuthStore::for_test("test").await;
    auth.seed_test_workspace("alpha").await;
    let state = AppState::durable(
        EventBus::in_process(),
        ClusterBus::standalone("creation-fence-test".into()),
        TrafficRecorder::default(),
        auth.pool(),
    );
    let create = || AgentCommand::Create {
        agent_id: "agent-reserved".into(),
        workload_credential: "fixture-only".into(),
        request: serde_json::from_value(serde_json::json!({
            "kind":"command", "name":"reserved", "cwd":".", "args":["fixture"]
        }))
        .unwrap(),
    };
    let caps = [treer_protocol::CAPABILITY_WORKLOAD_CREATE_PLACEMENT];
    let (_, mut first_rx) = connect(&state, &caps).await;
    let waiting = spawn_command(&state, create());
    // Already queued and sent before the durable tombstone is written.
    expect_envelope(first_rx.recv().await.unwrap());
    auth.delete_agent("alpha", "agent-reserved").await.unwrap();
    let (replacement_id, mut replacement_rx) = connect(&state, &caps).await;
    state
        .apply_snapshot(replacement_id, snapshot_of(test_server(), vec![]))
        .await
        .unwrap();
    let denied = |r: Result<serde_json::Value, ProtocolError>| r.unwrap_err().code;
    assert_eq!(denied(waiting.await.unwrap()), "agent_identity_denied");
    assert!(state.inner.pending.lock().await.is_empty());
    assert_eq!(
        denied(state.send_command("alpha", "server", create()).await),
        "agent_identity_denied"
    );
    assert_eq!(
        denied(
            state
                .handle_cluster_command("alpha", "server", replacement_id, "fwd".into(), create())
                .await
        ),
        "agent_identity_denied"
    );
    // PG outages fail closed; do not trust an empty/stale in-memory projection.
    auth.pool().close().await;
    assert_eq!(
        denied(state.send_command("alpha", "server", create()).await),
        "identity_check_unavailable"
    );
    assert!(
        replacement_rx.try_recv().is_err(),
        "revoked Create must never be replayed or dispatched"
    );
}

#[tokio::test]
async fn command_results_correlate_by_id_and_only_from_the_current_owner() {
    let state = AppState::new();
    let (owner, mut rx) = connect(
        &state,
        &[
            treer_protocol::CAPABILITY_MACHINE_EXEC,
            treer_protocol::CAPABILITY_AGENT_STARTUP_ADMIN,
        ],
    )
    .await;
    let (intruder_tx, _intruder_rx) = mpsc::unbounded_channel();
    let intruder_id = Uuid::new_v4();
    state
        .register_server(server_in("alpha", "intruder"), intruder_id, intruder_tx)
        .await
        .unwrap();
    let exec = || AgentCommand::Exec {
        request: treer_protocol::MachineExecRequest {
            cwd: ".".into(),
            command: "fixture".into(),
            args: vec![],
            timeout_ms: 1000,
        },
    };
    let mut waiting = Vec::new();
    let mut ids = Vec::new();
    for command in [
        exec(),
        exec(),
        AgentCommand::StartupAdminGet {
            agent_id: "fixture-agent".into(),
        },
    ] {
        waiting.push(spawn_command(&state, command));
        ids.push(expect_envelope(rx.recv().await.unwrap()).command_id);
    }
    // Another machine or a stale connection cannot complete the command.
    let forged = CommandResult::success(ids[0].clone(), serde_json::json!({"fake":true}));
    assert_eq!(
        state
            .complete_command_from("alpha", "intruder", intruder_id, forged.clone())
            .await
            .unwrap_err()
            .code,
        "command_identity_mismatch"
    );
    assert_eq!(
        state
            .complete_command_from("alpha", "server", Uuid::new_v4(), forged)
            .await
            .unwrap_err()
            .code,
        "duplicate_machine_connection"
    );
    // Results arrive out of order; duplicates and unknown ids consume nothing.
    for index in [2, 1, 0] {
        let result = CommandResult::success(ids[index].clone(), serde_json::json!({"i":index}));
        for result in [
            result.clone(),
            result,
            CommandResult::success("unknown", serde_json::json!({})),
        ] {
            state
                .complete_command_from("alpha", "server", owner, result)
                .await
                .unwrap();
        }
        assert_eq!(state.inner.pending.lock().await.len(), index);
    }
    for (index, result) in waiting.into_iter().enumerate() {
        assert_eq!(
            result.await.unwrap().unwrap(),
            serde_json::json!({"i":index})
        );
    }
}

#[tokio::test]
async fn controllers_without_a_capability_never_receive_its_commands() {
    use treer_protocol::*;
    let startup = || ReplaceAgentStartupRequest {
        cwd: None,
        command: "sh".into(),
        args: vec![],
    };
    let target = || "target".to_string();
    let cases: Vec<(Vec<&str>, AgentCommand, &str)> = vec![
        (
            vec![],
            AgentCommand::CreationPlacementCheck {},
            CAPABILITY_WORKLOAD_CREATE_PLACEMENT,
        ),
        (
            vec![CAPABILITY_SERVICE_HTTP_FETCH],
            AgentCommand::FetchServiceHttp {
                port: 8888,
                path: "/v1/policy/bundle?max_schema_version=2".into(),
                max_bytes: 1024,
                timeout_ms: 1000,
                policy_schema_version: Some(2),
            },
            CAPABILITY_POLICY_PATH_RULES,
        ),
        (
            vec![],
            AgentCommand::SubmitPrompt {
                agent_id: target(),
                request: SubmitAgentPromptRequest {
                    text: "task".into(),
                    delivery: PromptDelivery::Queue,
                    notify: None,
                    require_receipt: false,
                },
                sender: None,
            },
            CAPABILITY_AGENT_PROMPT_DELIVERY,
        ),
        (
            vec![],
            AgentCommand::QueueCancel {
                agent_id: target(),
                submission_id: "sub-1".into(),
            },
            CAPABILITY_AGENT_PROMPT_DELIVERY,
        ),
        (
            vec![CAPABILITY_AGENT_STARTUP, CAPABILITY_AGENT_LIFECYCLE],
            AgentCommand::StartupAdminSet {
                agent_id: target(),
                request: startup(),
            },
            CAPABILITY_AGENT_STARTUP_ADMIN,
        ),
        (
            vec![CAPABILITY_AGENT_STARTUP, CAPABILITY_AGENT_LIFECYCLE],
            AgentCommand::RestartWithStartup {
                agent_id: target(),
                request: startup(),
            },
            CAPABILITY_AGENT_STARTUP_ADMIN,
        ),
        (
            vec![CAPABILITY_AGENT_LIFECYCLE],
            AgentCommand::AppStartupUpdate {
                agent_id: "app_stable".into(),
                request: startup(),
                revision: 1,
            },
            CAPABILITY_APP_UPDATE,
        ),
        (
            vec![CAPABILITY_AGENT_LIFECYCLE],
            serde_json::from_value(serde_json::json!({
                "action":"app_ensure_lifecycle", "agent_id":"app_stable", "workload_credential":null,
                "enabled":true, "start_missing":true, "revision":1,
                "request":{"kind":"app", "name":"stable", "cwd":".", "args":["sh"]}
            }))
            .unwrap(),
            CAPABILITY_APP_UPDATE,
        ),
        (vec![], AgentCommand::Abort { agent_id: target() }, ""),
    ];
    for (capabilities, command, required) in cases {
        let state = AppState::new();
        let (_, mut rx) = connect(&state, &capabilities).await;
        let error = state
            .send_command("alpha", "server", command.clone())
            .await
            .unwrap_err();
        assert_eq!(error.code, "unsupported_command", "{command:?}");
        assert!(error.message.contains(required), "{}", error.message);
        assert!(rx.try_recv().is_err(), "{command:?} must not be sent");
        assert_eq!(state.snapshot("alpha").await.unwrap().servers.len(), 1);
    }
    // A refused command does not cost the connection its supported commands.
    let state = AppState::new();
    let (_, mut rx) = connect(&state, &[CAPABILITY_AGENT_STARTUP]).await;
    assert!(state
        .send_command(
            "alpha",
            "server",
            AgentCommand::StartupAdminGet { agent_id: target() }
        )
        .await
        .is_err());
    let waiting = spawn_command(&state, AgentCommand::StartupGet { agent_id: target() });
    let envelope = expect_envelope(rx.recv().await.unwrap());
    state
        .complete_command(CommandResult::success(
            envelope.command_id,
            serde_json::json!({"ok":true}),
        ))
        .await;
    assert_eq!(waiting.await.unwrap().unwrap()["ok"], true);
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
    state.rename_workspace_info(renamed.clone()).await.unwrap();
    let event = events.recv().await.unwrap();
    assert_eq!(
        (event.event.as_str(), &event.data["name"]),
        ("workspace.renamed", &serde_json::json!("Renamed"))
    );
    // A runtime ensure never reverts the stored name.
    state.ensure_workspace("alpha", "alpha").await;
    assert_eq!(state.snapshot("alpha").await.unwrap().workspace, renamed);
    let replicated = WorkspaceInfo {
        name: "Replicated".to_string(),
        ..renamed
    };
    state
        .apply_cluster_projection(ClusterProjectionUpdate::WorkspaceUpsert {
            workspace: replicated.clone(),
        })
        .await;
    assert_eq!(state.snapshot("alpha").await.unwrap().workspace, replicated);
    assert_eq!(events.recv().await.unwrap().event, "workspace.renamed");
}

#[tokio::test]
async fn proxy_messages_broadcast_only_to_their_workspace() {
    let state = AppState::new();
    let (_, mut alpha_rx) = connect_current(&state).await;
    let (beta_tx, mut beta_rx) = mpsc::unbounded_channel();
    state
        .register_server(server_in("beta", "server"), Uuid::new_v4(), beta_tx)
        .await
        .unwrap();
    let message = ProxyMessage::VirtualNetworkHosts {
        snapshot: treer_protocol::VirtualNetworkHostsSnapshot {
            workspace_id: "alpha".to_string(),
            revision: 4,
            hosts: Vec::new(),
        },
    };
    state.broadcast_proxy_message("alpha", &message).await;
    let received: ProxyMessage =
        serde_json::from_str(&expect_text(alpha_rx.recv().await.unwrap())).unwrap();
    assert_eq!(received, message);
    assert!(beta_rx.try_recv().is_err());
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
        .unwrap();
    let event = events.recv().await.unwrap();
    assert_eq!(event.schema_version, DOMAIN_EVENT_SCHEMA_VERSION);
    assert_eq!(
        (event.workspace_id.as_str(), event.action.as_str()),
        ("alpha", "server.updated")
    );
    assert_eq!(event.workspace_revision, Some(1));
    assert_eq!(event.payload["server_id"], "server");
}

#[tokio::test]
async fn agent_targets_accept_ids_and_unique_names_only() {
    let state = AppState::new();
    state.ensure_workspace("alpha", "Alpha").await;
    insert_agent(&state, test_agent("agent-1", "reviewer")).await;
    insert_agent(&state, test_agent("agent-2", "writer")).await;
    assert_eq!(
        state.resolve_agent("alpha", "agent-1").await.unwrap().name,
        "reviewer"
    );
    assert_eq!(
        state
            .resolve_agent("alpha", "writer")
            .await
            .unwrap()
            .agent_id,
        "agent-2"
    );
    insert_agent(&state, test_agent("agent-3", "reviewer")).await;
    assert_eq!(
        state
            .resolve_agent("alpha", "reviewer")
            .await
            .unwrap_err()
            .code,
        "agent_ambiguous"
    );
}

#[tokio::test]
async fn renames_and_deletes_survive_controller_snapshots_and_events() {
    let state = AppState::new();
    let (connection, _rx) = connect_current(&state).await;
    let agents = || {
        vec![
            test_agent("agent-1", "original-agent"),
            test_agent("agent-2", "doomed"),
        ]
    };
    state
        .apply_snapshot(connection, snapshot_of(test_server(), agents()))
        .await
        .unwrap();
    state
        .rename_server("alpha", "server", "renamed-machine".to_string())
        .await
        .unwrap();
    state
        .rename_agent("alpha", "agent-1", "renamed-agent".to_string())
        .await
        .unwrap();
    state.delete_agent("alpha", "agent-2").await.unwrap();
    // The Controller keeps reporting the old names and the deleted Agent.
    state
        .apply_snapshot(connection, snapshot_of(test_server(), agents()))
        .await
        .unwrap();
    for agent in agents() {
        state.apply_agent_event(connection, agent).await.unwrap();
    }
    let snapshot = state.snapshot("alpha").await.unwrap();
    assert_eq!(snapshot.servers[0].name, "renamed-machine");
    assert_eq!(snapshot.agents.len(), 1);
    assert_eq!(snapshot.agents[0].name, "renamed-agent");
    assert_eq!(
        state
            .resolve_agent("alpha", "agent-2")
            .await
            .unwrap_err()
            .code,
        "agent_not_found"
    );
}

#[tokio::test]
async fn deleting_server_closes_resources_and_blocks_reconnect_until_reenrollment() {
    let state = AppState::new();
    let (connection, mut server_rx) = connect_current(&state).await;
    state
        .apply_snapshot(
            connection,
            snapshot_of(test_server(), vec![test_agent("agent-1", "helper")]),
        )
        .await
        .unwrap();
    let (terminal_tx, mut terminal_rx) = mpsc::channel(TERMINAL_BROWSER_QUEUE_CAPACITY);
    state
        .attach_terminal("alpha", "agent-1", 120, 40, None, terminal_tx)
        .await
        .unwrap();
    expect_text(server_rx.recv().await.unwrap());
    let pending = spawn_command(&state, read_cmd());
    expect_envelope(server_rx.recv().await.unwrap());

    let (deleted, agents) = state.delete_server("alpha", "server").await.unwrap();
    assert_eq!((deleted.server_id.as_str(), agents.len()), ("server", 1));
    assert_eq!(server_rx.recv().await, Some(SocketFrame::Close));
    assert_eq!(pending.await.unwrap().unwrap_err().code, "server_deleted");
    let closed: TerminalServerMessage =
        serde_json::from_str(&expect_text(terminal_rx.recv().await.unwrap())).unwrap();
    assert_eq!(
        closed,
        TerminalServerMessage::Closed {
            reason: Some("machine deleted".to_string()),
            exit_code: None,
        }
    );
    let snapshot = state.snapshot("alpha").await.unwrap();
    assert!(snapshot.servers.is_empty() && snapshot.agents.is_empty());

    let reconnect = || async {
        let (tx, _rx) = mpsc::unbounded_channel();
        state
            .register_server(test_server(), Uuid::new_v4(), tx)
            .await
    };
    assert_eq!(reconnect().await.unwrap_err().code, "server_deleted");
    state.allow_server_reenrollment("alpha", "server").await;
    reconnect().await.expect("re-enrolled machine reconnects");
}

#[tokio::test]
async fn pending_commands_replay_to_a_replacement_only_when_safe() {
    let file_transfer = AgentCommand::FileTransfer {
        actor: treer_protocol::MachineTransferActor {
            workspace_id: "alpha".into(),
            kind: "machine".into(),
            id: "server".into(),
        },
        authorized_path: "result".into(),
        request: treer_protocol::MachineFileTransferRequest::PutBegin {
            path: "result".into(),
            overwrite: false,
            mode: 0o600,
            size: Some(3),
            limit: 64,
        },
    };
    let file_caps = [treer_protocol::CAPABILITY_MACHINE_FILE_TRANSFER];
    let full_caps: Vec<&str> = treer_protocol::CONTROLLER_CAPABILITIES.to_vec();
    // (command, replacement capabilities, expected outcome: None = replayed)
    let cases: [(AgentCommand, &[&str], Option<&str>); 3] = [
        (read_cmd(), &full_caps, None),
        (
            AgentCommand::Abort {
                agent_id: "agent-1".to_string(),
            },
            &[],
            Some("unsupported_command"),
        ),
        (file_transfer, &file_caps, Some("machine_transfer_stale")),
    ];
    for (command, replacement_caps, expected) in cases {
        let state = AppState::new();
        let (first, mut first_rx) = connect(&state, &full_caps).await;
        let waiting = spawn_command(&state, command);
        let first_envelope = expect_envelope(first_rx.recv().await.unwrap());
        state.disconnect_server("alpha", "server", first).await;
        let (second, mut second_rx) = connect(&state, replacement_caps).await;
        // Registration alone does not replay; the initial snapshot does.
        assert!(second_rx.try_recv().is_err());
        state
            .apply_snapshot(second, snapshot_of(test_server(), vec![]))
            .await
            .unwrap();
        match expected {
            None => {
                let replayed = expect_envelope(second_rx.recv().await.unwrap());
                assert_eq!(replayed.command_id, first_envelope.command_id);
                state
                    .complete_command(CommandResult::success(
                        replayed.command_id,
                        serde_json::json!({"replayed": true}),
                    ))
                    .await;
                assert_eq!(
                    waiting.await.unwrap().unwrap(),
                    serde_json::json!({"replayed": true})
                );
            }
            Some(code) => {
                assert_eq!(waiting.await.unwrap().unwrap_err().code, code);
                assert!(second_rx.try_recv().is_err());
            }
        }
    }
}

#[tokio::test]
async fn replacement_controller_fences_the_old_connection_and_original_can_reclaim() {
    let state = AppState::new();
    let register = |instance: &'static str| {
        let state = state.clone();
        async move {
            let connection = Uuid::new_v4();
            let (tx, rx) = mpsc::unbounded_channel();
            state
                .register_server_instance(test_server(), connection, instance.to_string(), tx)
                .await
                .unwrap();
            (connection, rx)
        }
    };
    let (first, mut first_rx) = register("ctl_1").await;
    let (second, _second_rx) = register("ctl_2").await;
    let error: ProxyMessage =
        serde_json::from_str(&expect_text(first_rx.recv().await.unwrap())).unwrap();
    assert!(matches!(
        error,
        ProxyMessage::Error { error } if error.code == "duplicate_machine_connection"
    ));
    assert_eq!(first_rx.recv().await, Some(SocketFrame::Close));
    assert_eq!(
        state
            .heartbeat("alpha", "server", first)
            .await
            .unwrap_err()
            .code,
        "duplicate_machine_connection"
    );
    state.heartbeat("alpha", "server", second).await.unwrap();
    state.disconnect_server("alpha", "server", second).await;
    let (reclaim, _reclaim_rx) = register("ctl_1").await;
    state.heartbeat("alpha", "server", reclaim).await.unwrap();
}

#[tokio::test]
async fn browser_network_streams_bridge_data_and_close_cleanly() {
    client_network_streams_bridge_data_and_close_cleanly(false).await;
}

#[tokio::test]
async fn service_network_streams_preserve_machine_source_and_directional_accounting() {
    client_network_streams_bridge_data_and_close_cleanly(true).await;
}

async fn client_network_streams_bridge_data_and_close_cleanly(machine_source: bool) {
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
    let source = if machine_source {
        "source-server"
    } else {
        BROWSER_TRAFFIC_ENDPOINT
    };
    let traffic_class = if machine_source {
        TrafficClass::ServiceIngress
    } else {
        TrafficClass::AgentInterface
    };
    let opening = tokio::spawn(async move {
        if machine_source {
            opening_state
                .open_client_network_stream_cancellable(
                    "alpha",
                    "server",
                    NetworkConnectRequest {
                        source_server_id: source.into(),
                        source_agent_id: Some("source-agent".into()),
                        destination_agent_id: Some("agent-a".into()),
                        host: "127.0.0.1".into(),
                        port: 8080,
                    },
                    traffic_class,
                )
                .await
                .map(|(stream, _)| stream)
        } else {
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
        }
    });
    let open = expect_network(server_rx.recv().await.expect("browser open frame"));
    assert_eq!(open.kind, NetworkBinaryKind::Open);
    let request: NetworkConnectRequest =
        serde_json::from_slice(&open.payload).expect("decode browser connect request");
    assert_eq!(request.host, "127.0.0.1");
    assert_eq!(request.port, 8080);
    assert_eq!(request.source_server_id, source);
    assert_eq!(
        request.source_agent_id.as_deref(),
        machine_source.then_some("source-agent")
    );
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
        traffic.pending_for("alpha", traffic_class, source, "server"),
        (7, 1)
    );
    assert_eq!(
        traffic.pending_for("alpha", traffic_class, "server", source),
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
        frame: SocketFrame::Text(serde_json::to_string(&ready).unwrap()),
    };
    state
        .handle_cluster_session_delivery(delivery.clone())
        .await
        .unwrap();
    browser_rx.try_recv().expect("ready reaches browser");
    state
        .handle_cluster_session_delivery(ClusterSessionDelivery {
            revision: Some(5),
            cursor: true,
            frame: SocketFrame::Binary(b"live".to_vec()),
            ..delivery
        })
        .await
        .unwrap();
    assert_eq!(
        browser_rx.try_recv().unwrap(),
        SocketFrame::Binary(b"live".to_vec())
    );
    let cursor: TerminalServerMessage =
        serde_json::from_str(&expect_text(browser_rx.try_recv().unwrap())).unwrap();
    assert_eq!(
        cursor,
        TerminalServerMessage::Cursor {
            stream_epoch: "remote-epoch".to_string(),
            revision: 5,
        }
    );
}

/// A connected `alpha/server` Controller with one shell Agent and an attached
/// terminal. Returns (connection, controller rx, browser rx, session id).
async fn attached_terminal(
    state: &AppState,
    browser_capacity: usize,
    cursor: Option<TerminalCursor>,
) -> (
    Uuid,
    mpsc::UnboundedReceiver<SocketFrame>,
    mpsc::Receiver<SocketFrame>,
    String,
) {
    let (connection, mut server_rx) = connect_current(state).await;
    insert_agent(state, test_agent("agent-1", "shell")).await;
    let (browser_tx, browser_rx) = mpsc::channel(browser_capacity);
    let session_id = state
        .attach_terminal("alpha", "agent-1", 120, 40, cursor.clone(), browser_tx)
        .await
        .unwrap();
    let attach: ProxyMessage =
        serde_json::from_str(&expect_text(server_rx.recv().await.unwrap())).unwrap();
    assert!(matches!(
        attach,
        ProxyMessage::TerminalAttach { session_id: ref attached, cursor: ref sent, .. }
            if attached == &session_id && sent == &cursor
    ));
    (connection, server_rx, browser_rx, session_id)
}

#[tokio::test]
async fn terminal_routes_raw_binary_and_deduplicates_revisions() {
    let state = AppState::new();
    let cursor = TerminalCursor {
        stream_epoch: "stream_a".to_string(),
        revision: 12,
    };
    let (connection, mut server_rx, mut browser_rx, session_id) =
        attached_terminal(&state, TERMINAL_BROWSER_QUEUE_CAPACITY, Some(cursor)).await;
    let replay = vec![b'x'; TERMINAL_REPLAY_CHUNK_BYTES + 1];
    state
        .terminal_ready(
            "alpha",
            "server",
            connection,
            &session_id,
            TerminalReadyPayload {
                revision: 7,
                replay: replay.clone(),
                stream_epoch: Some("stream_a".to_string()),
                gap: false,
            },
        )
        .await
        .unwrap();
    let ready: TerminalServerMessage =
        serde_json::from_str(&expect_text(browser_rx.recv().await.unwrap())).unwrap();
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
    for chunk in [
        &replay[..TERMINAL_REPLAY_CHUNK_BYTES],
        &replay[TERMINAL_REPLAY_CHUNK_BYTES..],
    ] {
        assert_eq!(
            browser_rx.recv().await,
            Some(SocketFrame::Binary(chunk.to_vec()))
        );
    }
    for (revision, output) in [(7, b"duplicate".as_slice()), (8, b"live")] {
        state
            .terminal_output(
                "alpha",
                "server",
                connection,
                &session_id,
                revision,
                output.to_vec(),
            )
            .await
            .unwrap();
    }
    assert_eq!(
        browser_rx.recv().await,
        Some(SocketFrame::Binary(b"live".to_vec()))
    );
    let cursor: TerminalServerMessage =
        serde_json::from_str(&expect_text(browser_rx.recv().await.unwrap())).unwrap();
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
        .unwrap();
    let SocketFrame::Binary(encoded) = server_rx.recv().await.unwrap() else {
        panic!("expected binary terminal input")
    };
    let input = TerminalBinaryFrame::decode(&encoded).unwrap();
    assert_eq!(input.kind, TerminalBinaryKind::Input);
    assert_eq!(input.session_id, session_id);
    assert_eq!(input.payload, vec![0, 0xff, b'\r']);
}

#[tokio::test]
async fn slow_terminal_consumer_is_detached_when_its_queue_fills() {
    let state = AppState::new();
    let (connection, mut server_rx, mut browser_rx, session_id) =
        attached_terminal(&state, 1, None).await;
    state
        .inner
        .terminal_sessions
        .lock()
        .await
        .get_mut(&session_id)
        .unwrap()
        .stream_epoch = Some("stream_a".to_string());
    state
        .terminal_output(
            "alpha",
            "server",
            connection,
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
    let detach: ProxyMessage =
        serde_json::from_str(&expect_text(server_rx.recv().await.unwrap())).unwrap();
    assert_eq!(detach, ProxyMessage::TerminalDetach { session_id });
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
    state
        .apply_cluster_snapshot(ClusterServerSnapshot {
            owner: crate::cluster::ConnectionOwner {
                proxy_id: "remote-proxy".to_string(),
                connection_id: Uuid::new_v4(),
            },
            revision: 1,
            snapshot: snapshot_of(test_server(), vec![test_agent("agent", "stale-agent-name")]),
        })
        .await;
    assert_eq!(
        state.resolve_server("alpha", "server").await.unwrap().name,
        "persisted-server-name"
    );
    assert_eq!(
        state.resolve_agent("alpha", "agent").await.unwrap().name,
        "persisted-agent-name"
    );
}

async fn wait_until<F: std::future::Future<Output = bool>>(
    what: &str,
    mut check: impl FnMut() -> F,
) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !check().await {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
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
    let agent_id = format!("agent-{suffix}");
    let subject_prefix = format!("treer.test.cluster.{suffix}");
    let bus_a = ClusterBus::connect(
        &nats_url,
        format!("proxy-a-{suffix}"),
        subject_prefix.clone(),
    )
    .await
    .unwrap();
    let bus_b = ClusterBus::connect(&nats_url, format!("proxy-b-{suffix}"), subject_prefix)
        .await
        .unwrap();
    let state_a = AppState::with_backplanes(EventBus::in_process(), bus_a.clone());
    let state_b = AppState::with_backplanes(EventBus::in_process(), bus_b.clone());
    let workspace = state_a
        .ensure_workspace(&workspace_id, "Cluster test")
        .await;
    bus_a.start(state_a.clone()).await.unwrap();
    bus_a
        .broadcast_projection(ClusterProjectionUpdate::WorkspaceUpsert { workspace })
        .await
        .unwrap();
    bus_b.start(state_b.clone()).await.unwrap();
    wait_until(
        "late proxy replays durable workspace projection",
        || async { state_b.snapshot(&workspace_id).await.is_ok() },
    )
    .await;

    let source_connection = Uuid::new_v4();
    let destination_connection = Uuid::new_v4();
    let (source_tx, _source_rx) = mpsc::unbounded_channel();
    state_a
        .register_server(
            server_in(&workspace_id, &source_id),
            source_connection,
            source_tx,
        )
        .await
        .unwrap();
    let (destination_tx, mut destination_rx) = mpsc::unbounded_channel();
    let destination = server_in(&workspace_id, &destination_id);
    state_b
        .register_server(destination.clone(), destination_connection, destination_tx)
        .await
        .unwrap();
    state_b
        .apply_snapshot(
            destination_connection,
            snapshot_of(
                destination,
                vec![agent_in(
                    &workspace_id,
                    &destination_id,
                    &agent_id,
                    "remote-agent",
                )],
            ),
        )
        .await
        .unwrap();
    wait_until("live projections replicate", || async {
        state_a
            .resolve_agent(&workspace_id, &agent_id)
            .await
            .is_ok()
            && state_b
                .resolve_server(&workspace_id, &source_id)
                .await
                .is_ok()
    })
    .await;

    state_a
        .rename_agent(&workspace_id, &agent_id, "renamed-remote-agent".to_string())
        .await
        .unwrap();
    state_a
        .rename_server(
            &workspace_id,
            &destination_id,
            "renamed-destination".to_string(),
        )
        .await
        .unwrap();
    wait_until("rename projections replicate", || async {
        state_b
            .resolve_agent(&workspace_id, &agent_id)
            .await
            .is_ok_and(|a| a.name == "renamed-remote-agent")
            && state_b
                .resolve_server(&workspace_id, &destination_id)
                .await
                .is_ok_and(|s| s.name == "renamed-destination")
    })
    .await;

    let command = tokio::spawn({
        let (state, workspace, server) = (
            state_a.clone(),
            workspace_id.clone(),
            destination_id.clone(),
        );
        async move {
            state
                .send_command(&workspace, &server, AgentCommand::ShutdownMachine)
                .await
        }
    });
    let envelope = expect_envelope(destination_rx.recv().await.unwrap());
    state_b
        .complete_command(CommandResult::success(
            envelope.command_id,
            serde_json::json!({"accepted": true}),
        ))
        .await;
    assert_eq!(
        command.await.unwrap().unwrap(),
        serde_json::json!({"accepted": true})
    );

    let (browser_tx, mut browser_rx) = mpsc::channel(TERMINAL_BROWSER_QUEUE_CAPACITY);
    let session_id = state_a
        .attach_terminal(&workspace_id, &agent_id, 100, 30, None, browser_tx)
        .await
        .unwrap();
    let attach: ProxyMessage =
        serde_json::from_str(&expect_text(destination_rx.recv().await.unwrap())).unwrap();
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
        .unwrap();
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
        .unwrap();
    assert_eq!(
        browser_rx.recv().await,
        Some(SocketFrame::Binary(b"live".to_vec()))
    );
    let cursor: TerminalServerMessage = serde_json::from_str(&expect_text(
        tokio::time::timeout(Duration::from_secs(5), browser_rx.recv())
            .await
            .expect("cross-proxy cursor arrives before the connection lease expires")
            .unwrap(),
    ))
    .unwrap();
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
        .unwrap();
    assert!(matches!(
        destination_rx.recv().await,
        Some(SocketFrame::Binary(_))
    ));

    let service_state = state_a.clone();
    let service_workspace = workspace_id.clone();
    let service_destination = destination_id.clone();
    let service_task = tokio::spawn(async move {
        service_state
            .open_browser_network_stream(
                &service_workspace,
                &service_destination,
                None,
                "127.0.0.1",
                3000,
                TrafficClass::ServiceIngress,
            )
            .await
            .unwrap()
    });
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
                payload: vec![],
            },
        )
        .await
        .unwrap();
    let mut service_stream = service_task.await.unwrap();
    service_stream.write_all(b"request").await.unwrap();
    let request = expect_network(destination_rx.recv().await.expect("service data"));
    assert_eq!(request.kind, NetworkBinaryKind::Data);
    assert_eq!(request.payload, b"request");
    state_b
        .relay_network_frame(
            &workspace_id,
            &destination_id,
            destination_connection,
            NetworkBinaryFrame {
                kind: NetworkBinaryKind::Data,
                stream_id: destination_open.stream_id.clone(),
                payload: b"response".to_vec(),
            },
        )
        .await
        .unwrap();
    let mut response = [0; 8];
    service_stream.read_exact(&mut response).await.unwrap();
    assert_eq!(&response, b"response");

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
    let mut tail = [0];
    let disconnected = tokio::time::timeout(Duration::from_secs(5), service_stream.read(&mut tail))
        .await
        .expect("remote disconnect closes explicit service");
    assert!(disconnected.is_err() || disconnected.unwrap() == 0);
    state_a
        .disconnect_server(&workspace_id, &source_id, source_connection)
        .await;
}

async fn read_model_capabilities(state: &AppState) -> Vec<String> {
    state
        .resolve_server("alpha", "server")
        .await
        .expect("server is known")
        .capabilities
}

#[tokio::test]
async fn cluster_replica_keeps_read_model_capabilities_when_the_owner_snapshot_has_none() {
    let state = AppState::new();
    state.ensure_workspace("alpha", "Alpha").await;
    let owner = crate::cluster::ConnectionOwner {
        proxy_id: "remote-proxy".to_string(),
        connection_id: Uuid::new_v4(),
    };
    let update = |revision: u64, capabilities: Vec<&str>| ClusterServerSnapshot {
        owner: owner.clone(),
        revision,
        snapshot: AgentServerSnapshot {
            server: ServerInfo {
                capabilities: capabilities.into_iter().map(str::to_string).collect(),
                ..test_server()
            },
            agents: Vec::new(),
        },
    };
    let upload = treer_protocol::CAPABILITY_MACHINE_UPLOAD;
    let v3 = treer_protocol::CAPABILITY_MACHINE_UPLOAD_V3;
    state
        .apply_cluster_snapshot(update(1, vec![upload, v3]))
        .await;
    assert_eq!(read_model_capabilities(&state).await, vec![upload, v3]);
    // The owning instance republishes after a Controller snapshot that
    // carries no capabilities: the replica keeps what it knows (#173).
    state.apply_cluster_snapshot(update(2, Vec::new())).await;
    assert_eq!(read_model_capabilities(&state).await, vec![upload, v3]);
    // An explicit list from the owner still replaces it (a re-registered
    // Controller without v3, e.g. Windows).
    state.apply_cluster_snapshot(update(3, vec![upload])).await;
    assert_eq!(read_model_capabilities(&state).await, vec![upload]);
}

#[tokio::test]
async fn disconnect_keeps_last_known_capabilities_and_reconnect_replaces_them() {
    let state = AppState::new();
    let upload = treer_protocol::CAPABILITY_MACHINE_UPLOAD;
    let v3 = treer_protocol::CAPABILITY_MACHINE_UPLOAD_V3;
    let first = Uuid::new_v4();
    let (first_tx, _first_rx) = mpsc::unbounded_channel();
    state
        .register_server_instance_with_capabilities(
            test_server(),
            first,
            "controller-1".to_string(),
            [upload, v3, treer_protocol::CAPABILITY_MACHINE_EXEC].map(str::to_string),
            first_tx,
        )
        .await
        .expect("register");
    // Only the allow-listed names reach the read model.
    assert_eq!(read_model_capabilities(&state).await, vec![upload, v3]);
    // While offline the last known list stays; the dialog gates on status.
    state.disconnect_server("alpha", "server", first).await;
    assert_eq!(read_model_capabilities(&state).await, vec![upload, v3]);
    // A legacy-only Controller reconnecting replaces the list ...
    let second = Uuid::new_v4();
    let (second_tx, _second_rx) = mpsc::unbounded_channel();
    state
        .register_server_instance_with_capabilities(
            test_server(),
            second,
            "controller-2".to_string(),
            [upload].map(str::to_string),
            second_tx,
        )
        .await
        .expect("re-register");
    assert_eq!(read_model_capabilities(&state).await, vec![upload]);
    // ... and its snapshots cannot add a capability the connection did not
    // declare, nor erase the one it did.
    state
        .apply_snapshot(
            second,
            AgentServerSnapshot {
                server: ServerInfo {
                    capabilities: vec![v3.to_string()],
                    ..test_server()
                },
                agents: Vec::new(),
            },
        )
        .await
        .expect("snapshot");
    assert_eq!(read_model_capabilities(&state).await, vec![upload]);
}
