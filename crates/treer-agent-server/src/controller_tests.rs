use super::*;

#[cfg(unix)]
#[tokio::test]
#[ignore = "requires cargo build -p treer-agent-host immediately before this isolated real-Host test"]
async fn recovery_real_host_cold_restart_preserves_intent_identity_and_argv() {
    use std::os::unix::fs::PermissionsExt;
    use treer_host_protocol::HostDaemonConfig;
    use treer_protocol::RuntimeRestoreState;
    struct IsolatedHost(tokio::process::Child);
    impl Drop for IsolatedHost {
        fn drop(&mut self) {
            if self.0.try_wait().ok().flatten().is_some() {
                return;
            }
            if let Some(pid) = self.0.id() {
                // Only direct children of this test's owned Host are targeted.
                let _ = std::process::Command::new("pkill")
                    .args(["-TERM", "-P", &pid.to_string()])
                    .status();
                let _ = std::process::Command::new("kill")
                    .args(["-TERM", &pid.to_string()])
                    .status();
            }
        }
    }
    let root = PathBuf::from("/tmp").join(format!(
        "trr-cold-{}",
        &Uuid::new_v4().simple().to_string()[..12]
    ));
    std::fs::create_dir_all(&root).unwrap();
    let socket = root.join("host.sock");
    let controller = root.join("idle-controller");
    std::fs::write(&controller, "#!/bin/sh\nexec sleep 120\n").unwrap();
    std::fs::set_permissions(&controller, std::fs::Permissions::from_mode(0o700)).unwrap();
    let config_path = root.join("host.json");
    std::fs::write(
        &config_path,
        serde_json::to_vec(&HostDaemonConfig {
            socket_path: socket.clone(),
            controller_path: controller,
            controller_config_path: root.join("unused.json"),
            root: root.clone(),
        })
        .unwrap(),
    )
    .unwrap();
    let binary =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/treer-agent-host");
    assert!(binary.is_file(), "build treer-agent-host first");
    async fn connect(socket: &std::path::Path, root: &std::path::Path) -> ControllerRuntime {
        let deadline = Instant::now() + Duration::from_secs(5);
        let (host, events) = loop {
            if let Ok(connection) = HostClient::connect(socket).await {
                break connection;
            }
            assert!(
                Instant::now() < deadline,
                "isolated Host socket did not appear"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        let sync = host.sync(Default::default()).await.unwrap();
        ControllerRuntime::from_sync(
            host,
            sync,
            events,
            ControllerConfig {
                workspace_id: "cold-workspace".into(),
                server_id: "cold-server".into(),
                agent_server_url: "http://127.0.0.1:1".into(),
                treer_binary: None,
                interface_cache_path: root.join("interfaces.json"),
                startup_store_path: root.join("startup.json"),
                root: root.into(),
            },
        )
        .unwrap()
        .0
    }
    async fn shutdown(child: &mut IsolatedHost) {
        std::process::Command::new("kill")
            .args(["-TERM", &child.0.id().unwrap().to_string()])
            .status()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), child.0.wait())
            .await
            .unwrap()
            .unwrap();
    }
    let spawn_host = || {
        IsolatedHost(
            tokio::process::Command::new(&binary)
                .arg("run")
                .arg("--config")
                .arg(&config_path)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .unwrap(),
        )
    };
    let mut first_host = spawn_host();
    let first = connect(&socket, &root).await;
    let literal = "literal $() ' spaces";
    let args = vec![
        "/bin/sh".into(),
        "-c".into(),
        "printf '%s\\n' \"$1\"; exec sleep 120".into(),
        "launcher".into(),
        literal.into(),
    ];
    for id in ["survivor", "stopped"] {
        first
            .create(
                &format!("create-{id}"),
                id.into(),
                new_workload_credential(),
                CreateAgentRequest {
                    server_id: None,
                    kind: "command".into(),
                    name: id.into(),
                    cwd: ".".into(),
                    args: args.clone(),
                    cols: 80,
                    rows: 24,
                    publish_ports: vec![],
                    recipe: None,
                },
            )
            .await
            .unwrap();
    }
    let before = first.get_startup("survivor").unwrap();
    assert_eq!(before.command, "/bin/sh");
    assert_eq!(before.args, args[1..]);
    first.stop("stop-intent", "stopped").await.unwrap();
    assert!(!first.get_startup("stopped").unwrap().enabled);
    shutdown(&mut first_host).await;
    let saved = StartupStore::load(root.join("startup.json")).unwrap();
    assert!(
        saved.get("survivor").unwrap().spec.enabled,
        "machine shutdown must preserve running intent"
    );
    assert!(!saved.get("stopped").unwrap().spec.enabled);
    let mut second_host = spawn_host();
    let second = connect(&socket, &root).await;
    let active = vec!["survivor".to_string(), "stopped".to_string()];
    let (a, b) = tokio::join!(
        second.restore_runtime(&active),
        second.restore_runtime(&active)
    );
    assert_eq!(
        a.items
            .iter()
            .find(|item| item.workload_id == "survivor")
            .unwrap()
            .state,
        RuntimeRestoreState::Started
    );
    assert_eq!(
        b.items
            .iter()
            .find(|item| item.workload_id == "stopped")
            .unwrap()
            .state,
        RuntimeRestoreState::SkippedStopped
    );
    let HostResponse::Synced { processes, .. } =
        second.inner.host.sync(Default::default()).await.unwrap()
    else {
        panic!("inventory");
    };
    assert_eq!(
        processes.iter().filter(|process| process.running).count(),
        1
    );
    assert_eq!(processes[0].process_id, "survivor");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let HostResponse::Output { replay } = second
            .inner
            .host
            .request(
                HostCommand::Read {
                    process_id: "survivor".into(),
                    cursor: None,
                },
                None,
            )
            .await
            .unwrap()
        else {
            panic!("read");
        };
        if plain_text(&replay).unwrap().contains(literal) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "literal argv did not reach restored process"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    shutdown(&mut second_host).await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn recovery_rechecks_host_and_preserves_stopped_and_revoked_identity() {
    use tokio::io::AsyncWriteExt;
    use treer_host_protocol::{
        decode_message, encode_message, HostBuildInfo, HostMessage, HostRequest,
    };
    use treer_protocol::RuntimeRestoreState;
    let root = PathBuf::from("/tmp").join(format!("trr-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&root).unwrap();
    let socket = root.join("host.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let sync = HostResponse::Synced {
        host_epoch: "new-host".into(),
        host_build: HostBuildInfo {
            version: "test".into(),
            git_commit: "test".into(),
        },
        processes: vec![],
        replay: vec![],
    };
    let host_sync = sync.clone();
    let spawn_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let host_spawn_count = spawn_count.clone();
    let inventory_failed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let host_inventory_failed = inventory_failed.clone();
    let host_task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut processes: Vec<HostProcessInfo> = vec![];
        while let Ok(length) = stream.read_u32().await {
            let mut bytes = vec![0; length as usize];
            stream.read_exact(&mut bytes).await.unwrap();
            let request: HostRequest = decode_message(&bytes).unwrap();
            let response = match request.command {
                HostCommand::Sync { .. } => {
                    if host_inventory_failed.load(std::sync::atomic::Ordering::SeqCst) {
                        for process in &mut processes {
                            process.running = false;
                            process.exit_code = Some(1);
                        }
                    }
                    let HostResponse::Synced {
                        host_epoch,
                        host_build,
                        replay,
                        ..
                    } = host_sync.clone()
                    else {
                        unreachable!()
                    };
                    HostResponse::Synced {
                        host_epoch,
                        host_build,
                        processes: processes.clone(),
                        replay,
                    }
                }
                HostCommand::Spawn { request } => {
                    assert_eq!(
                        request.process_id, "recover",
                        "stopped or revoked workload was spawned"
                    );
                    assert_eq!(request.command, "/bin/sh");
                    assert_eq!(request.cols, 80);
                    assert_eq!(request.rows, 24);
                    assert_eq!(
                        request.env.get("TREER_MACHINE_TOKEN").map(String::as_str),
                        Some("")
                    );
                    assert_eq!(
                        request
                            .env
                            .get("TREER_OPERATOR_CREDENTIAL")
                            .map(String::as_str),
                        Some("")
                    );
                    host_spawn_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let now = Utc::now();
                    let process = HostProcessInfo {
                        process_id: request.process_id,
                        pid: Some(1234),
                        cwd: request.cwd,
                        running: true,
                        started_at: now,
                        last_output_at: now,
                        exited_at: None,
                        exit_code: None,
                        stream_epoch: "stream".into(),
                        first_revision: 1,
                        next_revision: 1,
                        bracketed_paste: false,
                        metadata: request.metadata,
                    };
                    host_inventory_failed.store(false, std::sync::atomic::Ordering::SeqCst);
                    processes.retain(|old| old.process_id != process.process_id);
                    processes.push(process.clone());
                    HostResponse::Process { process }
                }
                _ => panic!("unexpected command"),
            };
            let message = HostMessage::Response {
                request_id: request.request_id,
                response: Some(response),
                error: None,
            };
            let encoded = encode_message(&message).unwrap();
            stream.write_u32(encoded.len() as u32).await.unwrap();
            stream.write_all(&encoded).await.unwrap();
        }
    });
    let (host, events) = HostClient::connect(&socket).await.unwrap();
    let (runtime, _) = ControllerRuntime::from_sync(
        host,
        sync,
        events,
        ControllerConfig {
            workspace_id: "workspace".into(),
            server_id: "server".into(),
            agent_server_url: "http://127.0.0.1:1".into(),
            treer_binary: None,
            interface_cache_path: root.join("interfaces.json"),
            startup_store_path: root.join("startup.json"),
            root: root.clone(),
        },
    )
    .unwrap();
    for (id, enabled) in [("stopped", false), ("revoked", true), ("recover", true)] {
        runtime
            .inner
            .startup_store
            .upsert(StoredAgentStartup {
                spec: AgentStartupSpec {
                    agent_id: id.into(),
                    server_id: "server".into(),
                    kind: "command".into(),
                    name: id.into(),
                    cwd: ".".into(),
                    command: "/bin/sh".into(),
                    args: vec![],
                    publish_ports: vec![],
                    enabled,
                    generation: 1,
                },
                workspace_id: "workspace".into(),
                workload_credential: new_workload_credential(),
                last_host_epoch: "old-host".into(),
                initial_writes: vec![],
                cols: 80,
                rows: 24,
            })
            .unwrap();
    }
    let active = vec!["recover".to_string()];
    let (first, second) = tokio::join!(
        runtime.restore_runtime(&active),
        runtime.restore_runtime(&active)
    );
    assert_eq!(first.items[0].state, RuntimeRestoreState::Started);
    assert!(matches!(
        second.items[0].state,
        RuntimeRestoreState::Started | RuntimeRestoreState::Attached
    ));
    assert_eq!(first.items[1].state, RuntimeRestoreState::IdentityRevoked);
    assert_eq!(first.items[2].state, RuntimeRestoreState::SkippedStopped);
    assert_eq!(spawn_count.load(std::sync::atomic::Ordering::SeqCst), 1);
    // Simulate a lost Controller ACK/cache: Host inventory must reattach.
    runtime.inner.agents.write().unwrap().clear();
    let attached = runtime.restore_runtime(&active).await;
    assert_eq!(attached.items[0].state, RuntimeRestoreState::Attached);
    assert_eq!(spawn_count.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(runtime.list()[0].agent_id, "recover");
    // Controller missed the exit event and still thinks this process runs.
    inventory_failed.store(true, std::sync::atomic::Ordering::SeqCst);
    let repaired = runtime.restore_runtime(&active).await;
    assert_eq!(repaired.items[0].state, RuntimeRestoreState::Started);
    assert_eq!(spawn_count.load(std::sync::atomic::Ordering::SeqCst), 2);
    let info = runtime.list()[0].clone();
    let mut completed = HostProcessInfo {
        process_id: info.agent_id.clone(),
        pid: info.pid,
        cwd: info.cwd.clone(),
        running: false,
        started_at: info.started_at,
        last_output_at: Utc::now(),
        exited_at: Some(Utc::now()),
        exit_code: Some(1),
        stream_epoch: "stream".into(),
        first_revision: 1,
        next_revision: 1,
        bracketed_paste: false,
        metadata: String::new(),
    };
    runtime.apply_process(completed.clone());
    assert!(
        runtime.get_startup("recover").unwrap().enabled,
        "failed Agents remain manually recoverable"
    );
    completed.exit_code = Some(0);
    runtime.apply_process(completed);
    assert!(
        !runtime.get_startup("recover").unwrap().enabled,
        "normal completion stops recovery intent"
    );
    let completed_report = runtime.restore_runtime(&active).await;
    assert_eq!(
        completed_report.items[0].state,
        RuntimeRestoreState::SkippedStopped
    );
    assert_eq!(spawn_count.load(std::sync::atomic::Ordering::SeqCst), 2);
    host_task.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn status_detector_prefers_blockers() {
    assert_eq!(
        detect_status("Working (esc to interrupt)\nAllow command?"),
        Some(AgentStatus::Blocked)
    );
}

#[test]
fn upload_names_reject_path_traversal() {
    assert!(validate_upload_file_name("notes.txt").is_ok());
    for name in [
        "",
        ".",
        "..",
        "../notes.txt",
        "dir/notes.txt",
        "dir\\notes.txt",
    ] {
        assert!(
            validate_upload_file_name(name).is_err(),
            "accepted {name:?}"
        );
    }
}

#[tokio::test]
async fn bounded_reader_drains_and_marks_truncation() {
    let input = &b"abcdefgh"[..];
    let (output, truncated) = read_bounded(input, 4).await.expect("read bytes");
    assert_eq!(output, b"abcd");
    assert!(truncated);
}

#[test]
fn status_detector_only_scans_recent_output() {
    let text = format!("Allow command?{}$", " ".repeat(STATUS_SCAN_LIMIT_BYTES));
    assert_eq!(detect_status(&text), Some(AgentStatus::Idle));
}

#[test]
fn output_trimming_uses_slack_and_preserves_utf8() {
    let mut below_slack = "x".repeat(OUTPUT_LIMIT_BYTES + OUTPUT_TRIM_SLACK_BYTES);
    trim_text(&mut below_slack);
    assert_eq!(
        below_slack.len(),
        OUTPUT_LIMIT_BYTES + OUTPUT_TRIM_SLACK_BYTES
    );

    let mut oversized = "é".repeat((OUTPUT_LIMIT_BYTES + OUTPUT_TRIM_SLACK_BYTES) / 2 + 1);
    trim_text(&mut oversized);
    assert_eq!(oversized.len(), OUTPUT_LIMIT_BYTES);
    assert_eq!(oversized.chars().count(), OUTPUT_LIMIT_BYTES / 2);
}

#[test]
fn workload_credentials_are_unique_and_compared_exactly() {
    let first = new_workload_credential();
    let second = new_workload_credential();
    assert!(first.starts_with("wlc_"));
    assert_eq!(first.len(), 68);
    assert_ne!(first, second);
    assert!(workload_credential_matches(&first, &first));
    assert!(!workload_credential_matches(&first, &second));
    assert!(!workload_credential_matches("", ""));
}

#[test]
fn host_metadata_preserves_the_workload_credential_for_controller_restarts() {
    let metadata = AgentMetadata {
        agent_id: "agent-a".to_string(),
        workspace_id: "workspace-a".to_string(),
        server_id: "server-a".to_string(),
        kind: "command".to_string(),
        name: "agent-a".to_string(),
        cwd: ".".to_string(),
        workload_credential: "wlc_secret".to_string(),
    };
    let encoded = serde_json::to_string(&metadata).expect("encode metadata");
    let restored: AgentMetadata = serde_json::from_str(&encoded).expect("restore metadata");
    assert_eq!(restored.workload_credential, "wlc_secret");
}

#[test]
fn prompt_text_uses_bracketed_paste_when_enabled() {
    assert_eq!(
        encode_prompt_text("hello", true),
        b"\x1b[200~hello\x1b[201~"
    );
    assert_eq!(encode_prompt_text("hello", false), b"hello");
}

#[test]
fn explicit_command_agents_still_spawn_directly() {
    let request = CreateAgentRequest {
        server_id: None,
        kind: "command".to_string(),
        name: "shell".to_string(),
        cwd: ".".to_string(),
        args: vec!["/bin/sh".to_string(), "-c".to_string(), "pwd".to_string()],
        cols: 120,
        rows: 36,
        publish_ports: Vec::new(),
        recipe: None,
    };

    let (_kind, launch) = resolve_launch(&request).expect("resolve command launch");

    assert_eq!(launch.command, "/bin/sh");
    assert_eq!(launch.args, ["-c", "pwd"]);
    assert!(launch.initial_writes.is_empty());
}

#[test]
fn empty_command_request_opens_an_unmodified_interactive_terminal() {
    let request = CreateAgentRequest {
        server_id: None,
        kind: "command".to_string(),
        name: "terminal".to_string(),
        cwd: ".".to_string(),
        args: Vec::new(),
        cols: 120,
        rows: 36,
        publish_ports: Vec::new(),
        recipe: None,
    };

    let (_kind, launch) = resolve_launch(&request).expect("resolve terminal launch");

    assert!(!launch.command.is_empty());
    assert_eq!(launch.args, ["-i"]);
    assert!(launch.initial_writes.is_empty());
}

#[test]
fn virtual_host_snapshots_only_move_forward_on_one_connection() {
    let current = VirtualNetworkHostsSnapshot {
        workspace_id: "default".to_string(),
        revision: 8,
        hosts: Vec::new(),
    };
    assert!(!should_replace_virtual_hosts(Some(&current), &current));
    assert!(!should_replace_virtual_hosts(
        Some(&current),
        &VirtualNetworkHostsSnapshot {
            revision: 7,
            ..current.clone()
        }
    ));
    assert!(should_replace_virtual_hosts(
        Some(&current),
        &VirtualNetworkHostsSnapshot {
            revision: 9,
            ..current.clone()
        }
    ));
}

#[test]
fn profiles_preserve_argv_without_shell_interpretation_or_installation() {
    let request = CreateAgentRequest {
        server_id: None,
        kind: "shell".into(),
        name: "profile".into(),
        cwd: ".".into(),
        args: vec![
            "arbitrary-agent".into(),
            "a b".into(),
            "$(touch forbidden)".into(),
            "".into(),
        ],
        cols: 80,
        rows: 24,
        publish_ports: vec![],
        recipe: None,
    };
    let (_, launch) = resolve_launch(&request).unwrap();
    assert_eq!(launch.command, "arbitrary-agent");
    assert_eq!(launch.args, request.args[1..]);
    assert!(launch.initial_writes.is_empty());
}

#[test]
fn provider_kinds_and_namespace_ports_require_explicit_migration() {
    let mut request = CreateAgentRequest {
        server_id: None,
        kind: "codex".into(),
        name: "legacy".into(),
        cwd: ".".into(),
        args: vec![],
        cols: 80,
        rows: 24,
        publish_ports: vec![],
        recipe: None,
    };
    assert_eq!(
        resolve_launch(&request).unwrap_err().code,
        "agent_kind_retired"
    );
    request.kind = "command".into();
    request.publish_ports = vec![3000];
    assert_eq!(
        resolve_launch(&request).unwrap_err().code,
        "sandbox_ports_retired"
    );
}
