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
    first.inner.workload_proxies.remove("survivor");
    first.inner.workload_proxies.remove("stopped");
    let saved = StartupStore::load(root.join("startup.json")).unwrap();
    assert!(
        saved.get("survivor").unwrap().spec.enabled,
        "machine shutdown must preserve running intent"
    );
    assert!(!saved.get("stopped").unwrap().spec.enabled);
    let mut second_host = spawn_host();
    let second = connect(&socket, &root).await;
    assert!(second
        .list()
        .iter()
        .any(|agent| agent.agent_id == "stopped" && agent.status.is_terminal()));
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
    let previous = second
        .list()
        .into_iter()
        .find(|agent| agent.agent_id == "survivor")
        .unwrap();
    let restarted = second
        .start("restart-survivor", "survivor", true)
        .await
        .unwrap();
    assert_eq!(restarted.agent_id, previous.agent_id);
    assert!(restarted.run_generation > previous.run_generation);
    assert_ne!(restarted.pid, previous.pid);
    let mut stale = processes[0].clone();
    stale.running = false;
    stale.exit_code = Some(0);
    second.apply_process(stale);
    let still_current = second
        .list()
        .into_iter()
        .find(|agent| agent.agent_id == "survivor")
        .unwrap();
    assert_eq!(still_current.pid, restarted.pid);
    assert!(!still_current.status.is_terminal());
    assert!(second.get_startup("survivor").unwrap().enabled);
    let (started, duplicate) = tokio::join!(
        second.start("start-stopped-a", "stopped", false),
        second.start("start-stopped-b", "stopped", false)
    );
    let started = started.unwrap();
    let duplicate = duplicate.unwrap();
    assert_eq!(started.pid, duplicate.pid);
    assert_eq!(started.run_generation, duplicate.run_generation);
    assert!(second.get_startup("stopped").unwrap().enabled);
    let credential = second
        .inner
        .startup_store
        .get("survivor")
        .unwrap()
        .workload_credential;
    // Replacement Controller takes over the exact listener recorded by Host.
    second.inner.workload_proxies.remove("survivor");
    second.inner.workload_proxies.remove("stopped");
    let hot = connect(&socket, &root).await;
    let attached = hot
        .list()
        .into_iter()
        .find(|agent| agent.agent_id == "survivor")
        .unwrap();
    assert_eq!(attached.pid, restarted.pid);
    assert_eq!(attached.run_generation, restarted.run_generation);
    assert_eq!(
        hot.inner
            .startup_store
            .get("survivor")
            .unwrap()
            .workload_credential,
        credential
    );
    let declaration = CreateAgentRequest {
        server_id: None,
        kind: "app".into(),
        name: "declared app".into(),
        cwd: ".".into(),
        args: vec!["/bin/sleep".into(), "120".into()],
        cols: 80,
        rows: 24,
        publish_ports: vec![],
        recipe: None,
    };
    let declared = hot
        .ensure_lifecycle(
            "declare-stopped",
            "declared".into(),
            Some(new_workload_credential()),
            declaration.clone(),
            false,
            false,
        )
        .await
        .unwrap();
    assert!(declared.status.is_terminal());
    assert!(declared.pid.is_none());
    assert_eq!(declared.run_generation, 0);
    let configured = hot
        .ensure_lifecycle(
            "configure-only",
            "configured".into(),
            Some(new_workload_credential()),
            declaration.clone(),
            true,
            false,
        )
        .await
        .unwrap();
    assert!(configured.pid.is_none());
    assert_eq!(configured.run_generation, 0);
    let once = hot
        .start("restart-configured", "configured", true)
        .await
        .unwrap();
    assert_eq!(
        once.run_generation, 1,
        "declaration-only restart launches once"
    );
    let live_declared = hot
        .ensure_lifecycle(
            "declare-live-only",
            "configured".into(),
            None,
            declaration.clone(),
            true,
            false,
        )
        .await
        .unwrap();
    assert_eq!(live_declared.pid, once.pid);
    assert!(!hot.get_startup("configured").unwrap().enabled);
    assert!(hot
        .restore_startup_agents(&["configured".into()])
        .await
        .is_empty());
    let live_started = hot
        .start("start-live-configured", "configured", false)
        .await
        .unwrap();
    assert_eq!(live_started.pid, once.pid);
    assert_eq!(live_started.run_generation, 1);
    assert!(hot.get_startup("configured").unwrap().enabled);
    hot.apply_stopped_startups(&["survivor".into()])
        .await
        .unwrap();
    assert!(!hot.get_startup("survivor").unwrap().enabled);
    assert!(
        hot.list()
            .into_iter()
            .find(|agent| agent.agent_id == "survivor")
            .unwrap()
            .status
            .is_terminal(),
        "central stop stops surviving ordinary process"
    );
    let redeclared = hot
        .ensure_lifecycle(
            "redeclare",
            "declared".into(),
            None,
            declaration,
            true,
            true,
        )
        .await
        .unwrap();
    assert!(
        redeclared.pid.is_none(),
        "bootstrap never overrides persisted stop intent"
    );
    let started = hot
        .start("explicit-start-declared", "declared", false)
        .await
        .unwrap();
    assert!(started.pid.is_some());
    assert_eq!(started.run_generation, 1);
    let adopted = hot
        .ensure_lifecycle(
            "adopt",
            "declared".into(),
            None,
            CreateAgentRequest {
                server_id: None,
                kind: "app".into(),
                name: "ignored".into(),
                cwd: ".".into(),
                args: vec!["/bin/false".into()],
                cols: 80,
                rows: 24,
                publish_ports: vec![],
                recipe: None,
            },
            true,
            true,
        )
        .await
        .unwrap();
    assert_eq!(adopted.pid, started.pid);
    assert_eq!(hot.get_startup("declared").unwrap().command, "/bin/sleep");
    let stopped_by_declaration = hot
        .ensure_lifecycle(
            "declare-offline-stop",
            "declared".into(),
            None,
            CreateAgentRequest {
                server_id: None,
                kind: "app".into(),
                name: "declared".into(),
                cwd: ".".into(),
                args: vec!["/bin/sleep".into(), "120".into()],
                cols: 80,
                rows: 24,
                publish_ports: vec![],
                recipe: None,
            },
            false,
            true,
        )
        .await
        .unwrap();
    assert!(stopped_by_declaration.status.is_terminal());
    assert!(!hot.get_startup("declared").unwrap().enabled);
    assert!(hot
        .restore_startup_agents(&["declared".into()])
        .await
        .is_empty());
    let app = hot
        .create(
            "create-app",
            "app-unit".into(),
            new_workload_credential(),
            CreateAgentRequest {
                server_id: None,
                kind: "app".into(),
                name: "app unit".into(),
                cwd: ".".into(),
                args: vec!["/bin/sh".into(), "-c".into(), "exit 0".into()],
                cols: 80,
                rows: 24,
                publish_ports: vec![],
                recipe: None,
            },
        )
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !hot
        .list()
        .iter()
        .any(|agent| agent.agent_id == "app-unit" && agent.status.is_terminal())
    {
        assert!(Instant::now() < deadline, "App did not exit");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        hot.get_startup("app-unit").unwrap().enabled,
        "App success retains always-restart intent"
    );
    let recovered = hot.restore_startup_agents(&["app-unit".into()]).await;
    assert!(recovered[0].1.as_ref().unwrap().run_generation > app.run_generation);
    hot.stop("stop-app", "app-unit").await.unwrap();
    assert!(!hot.get_startup("app-unit").unwrap().enabled);
    assert!(hot
        .restore_startup_agents(&["app-unit".into()])
        .await
        .is_empty());
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
    let root = std::env::temp_dir().join(format!("trr-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&root).unwrap();
    #[cfg(unix)]
    let socket = root.join("host.sock");
    #[cfg(windows)]
    let socket = treer_platform::endpoint_name(
        &Uuid::new_v4().to_string(),
        &treer_platform::current_user_sid().unwrap(),
    );
    let mut listener = treer_platform::LocalListener::bind(&socket).unwrap();
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
        let mut stream = listener.accept().await.unwrap();
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
            root: std::fs::canonicalize(&root).unwrap(),
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
        metadata: serde_json::to_string(&AgentMetadata {
            agent_id: info.agent_id.clone(),
            run_generation: info.run_generation,
            workspace_id: info.workspace_id.clone(),
            server_id: info.server_id.clone(),
            kind: info.kind.clone(),
            name: info.name.clone(),
            cwd: info.cwd.clone(),
            workload_credential: String::new(),
            http_proxy_port: None,
        })
        .unwrap(),
    };
    runtime
        .inner
        .startup_store
        .update("recover", |entry| entry.spec.generation += 1)
        .unwrap();
    completed.exit_code = Some(0);
    runtime.apply_process(completed.clone());
    assert!(
        runtime.get_startup("recover").unwrap().enabled,
        "old exit after next-generation intent is persisted cannot cancel restart"
    );
    runtime
        .inner
        .startup_store
        .update("recover", |entry| {
            entry.spec.generation = info.run_generation
        })
        .unwrap();
    completed.exit_code = Some(1);
    runtime.apply_process(completed.clone());
    assert!(
        runtime.get_startup("recover").unwrap().enabled,
        "failed Agents remain manually recoverable"
    );
    completed.exit_code = Some(0);
    runtime.apply_process(completed.clone());
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
    // A pre-upgrade live Host has no process generation, while its old startup
    // generation counted configuration edits. Initial adoption normalizes it.
    let mut legacy = completed;
    legacy.running = true;
    legacy.exit_code = None;
    legacy.exited_at = None;
    let mut metadata: serde_json::Value = serde_json::from_str(&legacy.metadata).unwrap();
    metadata.as_object_mut().unwrap().remove("run_generation");
    legacy.metadata = metadata.to_string();
    runtime
        .inner
        .startup_store
        .update("recover", |entry| {
            entry.spec.enabled = true;
            entry.spec.generation = 7;
        })
        .unwrap();
    runtime.inner.agents.write().unwrap().clear();
    runtime.migrate_legacy_live_generation(&legacy).unwrap();
    assert_eq!(runtime.get_startup("recover").unwrap().generation, 0);
    assert_eq!(
        runtime
            .upsert_process(legacy.clone(), None)
            .unwrap()
            .run_generation,
        0
    );
    let saved_legacy = runtime.inner.startup_store.get("recover").unwrap();
    runtime.inner.startup_store.remove("recover").unwrap();
    let registered = runtime
        .set_startup(
            "recover",
            SetAgentStartupRequest {
                cwd: ".".into(),
                command: saved_legacy.spec.command.clone(),
                args: saved_legacy.spec.args.clone(),
                publish_ports: vec![],
            },
        )
        .unwrap();
    assert_eq!(
        registered.generation, 0,
        "registering a legacy live process preserves generation zero"
    );
    runtime.inner.startup_store.upsert(saved_legacy).unwrap();
    legacy.running = false;
    legacy.exit_code = Some(0);
    legacy.exited_at = Some(Utc::now());
    runtime.apply_process(legacy.clone());
    assert!(runtime.list()[0].status.is_terminal());
    assert!(!runtime.get_startup("recover").unwrap().enabled);
    // The replacement persists its new generation before spawning. An old
    // legacy exit cannot be normalized again or cancel that running intent.
    runtime
        .inner
        .startup_store
        .update("recover", |entry| {
            entry.spec.enabled = true;
            entry.spec.generation = 1;
        })
        .unwrap();
    let entry = runtime.inner.startup_store.get("recover").unwrap();
    let replacement = runtime.spawn_startup(&entry).await.unwrap();
    assert_eq!(replacement.run_generation, 1);
    runtime.apply_process(legacy);
    assert!(runtime.get_startup("recover").unwrap().enabled);
    assert!(!runtime.list()[0].status.is_terminal());
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
        run_generation: 1,
        workspace_id: "workspace-a".to_string(),
        server_id: "server-a".to_string(),
        kind: "command".to_string(),
        name: "agent-a".to_string(),
        cwd: ".".to_string(),
        workload_credential: "wlc_secret".to_string(),
        http_proxy_port: Some(19001),
    };
    let encoded = serde_json::to_string(&metadata).expect("encode metadata");
    let restored: AgentMetadata = serde_json::from_str(&encoded).expect("restore metadata");
    assert_eq!(restored.workload_credential, "wlc_secret");
    assert_eq!(restored.http_proxy_port, Some(19001));
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
fn bridge_agents_spawn_directly_and_keep_prompt_control_in_the_bridge() {
    let request = CreateAgentRequest {
        server_id: None,
        kind: "bridge".to_string(),
        name: "bridge".to_string(),
        cwd: ".".to_string(),
        args: vec!["./bridge".to_string(), "--serve".to_string()],
        cols: 120,
        rows: 36,
        publish_ports: Vec::new(),
        recipe: None,
    };

    let (kind, launch) = resolve_launch(&request).expect("resolve bridge launch");

    assert_eq!(kind, "bridge");
    assert_eq!(launch.command, "./bridge");
    assert_eq!(launch.args, ["--serve"]);
    assert!(launch.initial_writes.is_empty());
}

#[test]
fn managed_apps_spawn_directly_with_explicit_service_registration() {
    let request = CreateAgentRequest {
        server_id: None,
        kind: "app".to_string(),
        name: "docs".to_string(),
        cwd: ".".to_string(),
        args: vec![
            "python3".to_string(),
            "-m".to_string(),
            "http.server".to_string(),
            "8080".to_string(),
        ],
        cols: 120,
        rows: 36,
        publish_ports: Vec::new(),
        recipe: None,
    };

    let (kind, launch) = resolve_launch(&request).expect("resolve App launch");

    assert_eq!(kind, "app");
    assert_eq!(launch.command, "python3");
    assert_eq!(launch.args, ["-m", "http.server", "8080"]);
    assert!(launch.initial_writes.is_empty());
    assert!(launch.publish_ports.is_empty());
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
