use super::*;

#[tokio::test]
async fn recovery_inventory_and_controller_replacement_retain_exited_output() {
    use tokio::io::AsyncWriteExt;
    use treer_host_protocol::{
        decode_message, encode_message, HostBuildInfo, HostMessage, HostRequest,
    };

    let root = std::env::temp_dir().join(format!("trr-output-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&root).unwrap();
    #[cfg(unix)]
    let socket = root.join("host.sock");
    #[cfg(windows)]
    let socket = treer_platform::endpoint_name(
        &Uuid::new_v4().to_string(),
        &treer_platform::current_user_sid().unwrap(),
    );
    let mut listener = treer_platform::LocalListener::bind(&socket).unwrap();
    // Host wire timestamps have millisecond precision on every inventory read.
    let now = chrono::DateTime::from_timestamp_millis(Utc::now().timestamp_millis()).unwrap();
    let mut processes = Vec::new();
    let mut replay = Vec::new();
    for kind in ["command", "app"] {
        let metadata = AgentMetadata {
            agent_id: kind.into(),
            run_generation: 1,
            workspace_id: "workspace".into(),
            server_id: "server".into(),
            kind: kind.into(),
            name: kind.into(),
            cwd: ".".into(),
            workload_credential: new_workload_credential(),
            http_proxy_port: None,
        };
        processes.push(HostProcessInfo {
            process_id: kind.into(),
            pid: Some(1234),
            cwd: ".".into(),
            running: true,
            started_at: now,
            last_output_at: now,
            exited_at: None,
            exit_code: None,
            stream_epoch: format!("epoch-{kind}"),
            first_revision: 1,
            next_revision: 3,
            bracketed_paste: false,
            metadata: serde_json::to_string(&metadata).unwrap(),
        });
        replay.push(HostOutputReplay {
            process_id: kind.into(),
            stream_epoch: format!("epoch-{kind}"),
            first_available_revision: 1,
            next_revision: 3,
            gap: false,
            chunks: vec![HostOutputChunk {
                process_id: kind.into(),
                stream_epoch: format!("epoch-{kind}"),
                revision: 2,
                data: format!("retained {kind} output\n").into_bytes(),
                bracketed_paste: false,
                emitted_at: now,
            }],
        });
    }
    let initial = HostResponse::Synced {
        host_epoch: "surviving-host".into(),
        host_build: HostBuildInfo {
            version: "test".into(),
            git_commit: "test".into(),
        },
        processes: processes.clone(),
        replay: replay.clone(),
    };
    for process in &mut processes {
        process.running = false;
        process.exited_at = Some(now);
        process.exit_code = Some(0);
    }
    let mut exited = initial.clone();
    if let HostResponse::Synced {
        processes: inventory,
        ..
    } = &mut exited
    {
        *inventory = processes.clone();
    }
    let inventory = exited.clone();
    let task = tokio::spawn(async move {
        let mut stream = listener.accept().await.unwrap();
        while let Ok(length) = stream.read_u32().await {
            let mut bytes = vec![0; length as usize];
            stream.read_exact(&mut bytes).await.unwrap();
            let request: HostRequest = decode_message(&bytes).unwrap();
            assert!(matches!(request.command, HostCommand::Sync { .. }));
            let encoded = encode_message(&HostMessage::Response {
                request_id: request.request_id,
                response: Some(inventory.clone()),
                error: None,
            })
            .unwrap();
            stream.write_u32(encoded.len() as u32).await.unwrap();
            stream.write_all(&encoded).await.unwrap();
        }
    });
    let (host, events) = HostClient::connect(&socket).await.unwrap();
    let config = || ControllerConfig {
        protected_paths: vec![],
        workspace_id: "workspace".into(),
        server_id: "server".into(),
        agent_server_url: "http://127.0.0.1:1".into(),
        proxy_public_url: "https://proxy.example/".into(),
        treer_binary: None,
        interface_cache_path: root.join("interfaces.json"),
        startup_store_path: root.join("startup.json"),
        private_state_root: root.join("agent-private"),
        provider_cli_root: root.join("provider-cli"),
        root: root.clone(),
    };
    let (runtime, _) =
        ControllerRuntime::from_sync(host.clone(), initial, events, config()).unwrap();
    assert_eq!(
        runtime.process_environment(Some(("command", "fixture")))["TREER_PROXY_URL"],
        "https://proxy.example/"
    );
    // Queue preflight must reject both terminal-only and legacy AIS Agents
    // without writing to their PTY (this Host only accepts inventory Sync).
    for capabilities in [vec![], vec!["prompt.submit".to_string()]] {
        if !capabilities.is_empty() {
            runtime
                .get("command")
                .unwrap()
                .lock()
                .unwrap()
                .info
                .interface = Some(AgentInterfaceDescriptor {
                origin_url: None,
                protocol: treer_protocol::AGENT_INTERFACE_PROTOCOL_V1.into(),
                instance_id: "legacy".into(),
                port: 1,
                capabilities,
                ui_path: None,
                registered_at: Utc::now(),
            });
        }
        let error = runtime
            .submit_prompt_delivery(
                "queue-preflight",
                "command",
                treer_protocol::SubmitAgentPromptRequest {
                    text: "must not insert".into(),
                    delivery: treer_protocol::PromptDelivery::Queue,
                    notify: None,
                    require_receipt: false,
                },
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, "queue_unsupported");
    }
    runtime
        .get("command")
        .unwrap()
        .lock()
        .unwrap()
        .info
        .interface = None;
    for process in &processes {
        runtime.apply_process(process.clone());
        let before = runtime.read(&process.process_id, None).unwrap();
        assert_eq!(
            before.text,
            format!("retained {} output\n", process.process_id)
        );
        assert_eq!(before.revision, 2);
    }
    for _ in 0..2 {
        assert!(runtime.restore_startup_agents(&[]).await.is_empty());
        for process in &processes {
            let after = runtime.read(&process.process_id, None).unwrap();
            assert_eq!(
                after.text,
                format!("retained {} output\n", process.process_id)
            );
            assert_eq!(after.revision, 2);
        }
    }
    // A replacement Controller reconstructs the same exited runs from the
    // still-running Host's bounded replay, then reconciles inventory again.
    let replacement_events = HostEvents {
        output: tokio::sync::broadcast::channel(1).1,
        processes: tokio::sync::broadcast::channel(1).1,
        disconnected: tokio::sync::watch::channel(false).1,
    };
    let replacement_sync = host.sync(Default::default()).await.unwrap();
    let (replacement, _) =
        ControllerRuntime::from_sync(host, replacement_sync, replacement_events, config()).unwrap();
    assert!(replacement.restore_startup_agents(&[]).await.is_empty());
    for process in &processes {
        let after = replacement.read(&process.process_id, None).unwrap();
        assert_eq!(
            after.text,
            format!("retained {} output\n", process.process_id)
        );
        assert_eq!(after.revision, 2);
    }
    for mut process in processes {
        replacement.apply_output(HostOutputChunk {
            process_id: process.process_id.clone(),
            stream_epoch: process.stream_epoch.clone(),
            revision: 3,
            data: b"late output\n".to_vec(),
            bracketed_paste: false,
            emitted_at: now,
        });
        // Sync can race the output relay; neither metadata-only inventory nor
        // an older replay may rewind a newer cached revision.
        replacement.upsert_process(process.clone(), None).unwrap();
        replacement
            .upsert_process(process.clone(), Some("stale replay\n".into()))
            .unwrap();
        let after = replacement.read(&process.process_id, None).unwrap();
        assert_eq!(after.revision, 3);
        assert!(after.text.ends_with("late output\n"));

        process.stream_epoch.push_str("-new-run");
        process.next_revision = 1;
        let mut metadata: AgentMetadata = serde_json::from_str(&process.metadata).unwrap();
        metadata.run_generation += 1;
        process.metadata = serde_json::to_string(&metadata).unwrap();
        replacement.upsert_process(process.clone(), None).unwrap();
        let new_run = replacement.read(&process.process_id, None).unwrap();
        assert!(new_run.text.is_empty());
        assert_eq!(new_run.revision, 0);
        assert_eq!(
            replacement
                .list()
                .iter()
                .find(|a| a.agent_id == process.process_id)
                .unwrap()
                .run_generation,
            2
        );
    }
    task.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn startup_admin_preserves_identity_credentials_and_fences_concurrent_restarts() {
    use tokio::io::AsyncWriteExt;
    use treer_host_protocol::{
        decode_message, encode_message, HostBuildInfo, HostMessage, HostRequest,
    };
    let root = std::env::temp_dir().join(format!(
        "trr-admin-{}",
        &Uuid::new_v4().simple().to_string()[..12]
    ));
    std::fs::create_dir_all(root.join("repo")).unwrap();
    let bin = root.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let executable_name = |name: &str| {
        if cfg!(windows) {
            format!("{name}.exe")
        } else {
            name.into()
        }
    };
    for name in ["old-command", "new-command", "fail-command"] {
        let path = bin.join(executable_name(name));
        std::fs::write(&path, b"mock Host executable").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
    // A workload-local decoy must never take precedence over absolute PATH.
    std::fs::write(
        root.join("repo").join(executable_name("old-command")),
        b"decoy",
    )
    .unwrap();
    let expected_new_command = bin.join(executable_name("new-command"));
    #[cfg(unix)]
    let socket = root.join("host.sock");
    #[cfg(windows)]
    let socket = treer_platform::endpoint_name(
        &Uuid::new_v4().to_string(),
        &treer_platform::current_user_sid().unwrap(),
    );
    let mut listener = treer_platform::LocalListener::bind(&socket).unwrap();
    let sync = HostResponse::Synced {
        host_epoch: "admin-test".into(),
        host_build: HostBuildInfo {
            version: "test".into(),
            git_commit: "test".into(),
        },
        processes: vec![],
        replay: vec![],
    };
    let host_sync = sync.clone();
    let spawns = Arc::new(Mutex::new(Vec::<HostSpawnRequest>::new()));
    let observed_spawns = spawns.clone();
    let saved_path = root.join("startup.json");
    let host_saved_path = saved_path.clone();
    let task = tokio::spawn(async move {
        let mut stream = listener.accept().await.unwrap();
        let mut processes: Vec<HostProcessInfo> = vec![];
        while let Ok(length) = stream.read_u32().await {
            let mut bytes = vec![0; length as usize];
            stream.read_exact(&mut bytes).await.unwrap();
            let request: HostRequest = decode_message(&bytes).unwrap();
            if matches!(&request.command, HostCommand::Spawn { request } if Path::new(&request.command).file_stem().and_then(|name| name.to_str()) == Some("fail-command"))
            {
                let encoded = encode_message(&HostMessage::Response {
                    request_id: request.request_id,
                    response: None,
                    error: Some(treer_host_protocol::HostError::new(
                        "spawn_failed",
                        "synthetic failure",
                    )),
                })
                .unwrap();
                stream.write_u32(encoded.len() as u32).await.unwrap();
                stream.write_all(&encoded).await.unwrap();
                continue;
            }
            let response = match request.command {
                HostCommand::Sync { .. } => {
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
                    let durable = StartupStore::load(host_saved_path.clone()).unwrap();
                    assert!(
                        durable.get(&request.process_id).is_some(),
                        "startup intent must be durable before any Host spawn"
                    );
                    assert!(!durable.is_denied(&request.process_id).unwrap());
                    observed_spawns.lock().unwrap().push(request.clone());
                    let now = Utc::now();
                    let metadata: AgentMetadata = serde_json::from_str(&request.metadata).unwrap();
                    let process = HostProcessInfo {
                        process_id: request.process_id,
                        pid: Some(1000 + metadata.run_generation as u32),
                        cwd: request.cwd,
                        running: true,
                        started_at: now,
                        last_output_at: now,
                        exited_at: None,
                        exit_code: None,
                        stream_epoch: format!("run-{}", metadata.run_generation),
                        first_revision: 1,
                        next_revision: 1,
                        bracketed_paste: false,
                        metadata: request.metadata,
                    };
                    processes.retain(|old| old.process_id != process.process_id);
                    processes.push(process.clone());
                    HostResponse::Process { process }
                }
                HostCommand::Stop { process_id } => {
                    let saved = StartupStore::load(host_saved_path.clone())
                        .unwrap()
                        .get(&process_id)
                        .unwrap();
                    assert!(
                        ["new-command", "fail-command"].contains(&saved.spec.command.as_str()),
                        "new argv must be durable before stopping the old process"
                    );
                    let process = processes
                        .iter_mut()
                        .find(|p| p.process_id == process_id)
                        .unwrap();
                    process.running = false;
                    process.exit_code = Some(0);
                    process.exited_at = Some(Utc::now());
                    HostResponse::Process {
                        process: process.clone(),
                    }
                }
                _ => panic!("unexpected command"),
            };
            let encoded = encode_message(&HostMessage::Response {
                request_id: request.request_id,
                response: Some(response),
                error: None,
            })
            .unwrap();
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
            protected_paths: vec![],
            workspace_id: "workspace".into(),
            server_id: "server".into(),
            agent_server_url: "http://127.0.0.1:1".into(),
            proxy_public_url: "https://proxy.example/".into(),
            treer_binary: Some(bin.join("treer")),
            interface_cache_path: root.join("interfaces.json"),
            startup_store_path: saved_path.clone(),
            private_state_root: root.join("agent-private"),
            provider_cli_root: root.join("provider-cli"),
            root: std::fs::canonicalize(&root).unwrap(),
        },
    )
    .unwrap();
    let credential = new_workload_credential();
    let missing = runtime
        .create(
            "missing-create",
            "missing-id".into(),
            credential.clone(),
            CreateAgentRequest {
                folder_id: None,
                server_id: None,
                kind: "command".into(),
                name: "missing".into(),
                cwd: "repo".into(),
                args: vec![format!("missing-cli-{}", Uuid::new_v4().simple())],
                cols: 80,
                rows: 24,
                publish_ports: vec![],
                recipe: None,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(missing.code, "command_not_found");
    let details = missing.details.as_ref().unwrap();
    assert_eq!(
        details.creation_state,
        treer_protocol::AgentCreationFailureState::NotStarted
    );
    assert_eq!(details.agent_id.as_deref(), Some("missing-id"));
    assert!(details
        .command
        .as_deref()
        .unwrap()
        .starts_with("missing-cli-"));
    assert!(runtime.inner.startup_store.get("missing-id").is_none());
    assert!(spawns.lock().unwrap().is_empty());
    let serialized = serde_json::to_string(&missing).unwrap();
    assert!(!serialized.contains(&root.to_string_lossy().to_string()));
    assert!(!serialized.contains(&credential));
    let denied_id = format!("ag_{}", Uuid::new_v4().simple());
    let late_request = || CreateAgentRequest {
        folder_id: None,
        server_id: None,
        kind: "command".into(),
        name: "late".into(),
        cwd: "repo".into(),
        args: vec!["old-command".into()],
        cols: 80,
        rows: 24,
        publish_ports: vec![],
        recipe: None,
    };
    // join polls fencing first: it holds the recovery lock while awaiting Host
    // inventory, so the racing Create cannot pass until denial is durable.
    let (fenced, late) = tokio::join!(
        runtime.fence_absent_agent_reservation(&denied_id),
        runtime.create(
            "late-create",
            denied_id.clone(),
            credential.clone(),
            late_request()
        ),
    );
    fenced.unwrap();
    assert_eq!(late.unwrap_err().code, "agent_identity_denied");
    runtime
        .fence_absent_agent_reservation(&denied_id)
        .await
        .unwrap();
    assert!(StartupStore::load(saved_path.clone())
        .unwrap()
        .is_denied(&denied_id)
        .unwrap());
    assert_eq!(
        runtime
            .ensure_lifecycle(
                "late-ensure",
                denied_id.clone(),
                Some(credential.clone()),
                late_request(),
                true,
                true
            )
            .await
            .unwrap_err()
            .code,
        "agent_identity_denied"
    );
    assert!(
        spawns.lock().unwrap().is_empty(),
        "denied identities never reach Host spawn"
    );
    let original = runtime
        .create(
            "create",
            "stable-id".into(),
            credential.clone(),
            CreateAgentRequest {
                folder_id: None,
                server_id: None,
                kind: "command".into(),
                name: "target".into(),
                cwd: "repo".into(),
                args: vec!["old-command".into()],
                cols: 80,
                rows: 24,
                publish_ports: vec![],
                recipe: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        runtime
            .fence_absent_agent_reservation("stable-id")
            .await
            .unwrap_err()
            .code,
        "agent_reservation_observed"
    );
    // Even if the Controller projection/startup are absent, Host inventory is
    // authoritative: an existing process must never be fenced as a reservation.
    let projected = runtime
        .inner
        .agents
        .write()
        .unwrap()
        .remove("stable-id")
        .unwrap();
    let saved = runtime.inner.startup_store.get("stable-id").unwrap();
    runtime.inner.startup_store.remove("stable-id").unwrap();
    assert_eq!(
        runtime
            .fence_absent_agent_reservation("stable-id")
            .await
            .unwrap_err()
            .code,
        "agent_reservation_observed"
    );
    assert!(!runtime.inner.startup_store.is_denied("stable-id").unwrap());
    runtime.inner.startup_store.upsert(saved).unwrap();
    runtime
        .inner
        .agents
        .write()
        .unwrap()
        .insert("stable-id".into(), projected);
    let generation = original.run_generation;
    assert_eq!(
        runtime.get_startup("stable-id").unwrap().command,
        "old-command"
    );
    assert_eq!(
        spawns.lock().unwrap()[0].command,
        bin.join(executable_name("old-command")).to_str().unwrap()
    );
    let replace = || ReplaceAgentStartupRequest {
        cwd: None,
        command: "new-command".into(),
        args: vec!["literal $() ' spaces".into()],
    };
    let saved = runtime
        .replace_startup("stable-id", replace())
        .await
        .unwrap();
    assert_eq!(saved.cwd, "repo");
    assert_eq!(saved.generation, generation);
    assert_eq!(
        spawns.lock().unwrap().len(),
        1,
        "set alone must not restart"
    );
    let before = runtime.get_startup("stable-id").unwrap();
    for invalid in [
        ReplaceAgentStartupRequest {
            cwd: None,
            command: " ".into(),
            args: vec![],
        },
        ReplaceAgentStartupRequest {
            cwd: Some("../outside".into()),
            ..replace()
        },
        ReplaceAgentStartupRequest {
            args: vec![credential.clone()],
            ..replace()
        },
    ] {
        assert!(runtime.replace_startup("stable-id", invalid).await.is_err());
        assert_eq!(runtime.get_startup("stable-id").unwrap(), before);
    }
    let (a, b) = tokio::join!(
        runtime.restart_with_startup("restart-a", "stable-id", replace()),
        runtime.start("restart-b", "stable-id", true)
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert_eq!(a.agent_id, "stable-id");
    assert_eq!(b.agent_id, "stable-id");
    assert_eq!(a.run_generation, generation + 1);
    assert_eq!(b.run_generation, generation + 2);
    let launches = spawns.lock().unwrap().clone();
    assert_eq!(launches.len(), 3);
    for launch in &launches[1..] {
        assert_eq!(launch.command, expected_new_command.to_str().unwrap());
        assert_eq!(launch.args, replace().args);
        assert_eq!(launch.env["TREER_WORKLOAD_CREDENTIAL"], credential);
        assert_eq!(launch.cols, 80);
        assert_eq!(launch.rows, 24);
    }
    let old = &launches[0];
    runtime.apply_process(HostProcessInfo {
        process_id: old.process_id.clone(),
        pid: original.pid,
        cwd: old.cwd.clone(),
        running: false,
        started_at: original.started_at,
        last_output_at: Utc::now(),
        exited_at: Some(Utc::now()),
        exit_code: Some(0),
        stream_epoch: "run-1".into(),
        first_revision: 1,
        next_revision: 1,
        bracketed_paste: false,
        metadata: old.metadata.clone(),
    });
    assert_eq!(runtime.list()[0].run_generation, generation + 2);
    assert!(runtime.get_startup("stable-id").unwrap().enabled);
    runtime
        .inner
        .startup_store
        .update("stable-id", |entry| entry.spec.enabled = false)
        .unwrap();
    // Stopped intent is still an established workload, even if the volatile
    // Controller projection disappeared. Cleanup must not fence its identity.
    let stopped_projection = runtime
        .inner
        .agents
        .write()
        .unwrap()
        .remove("stable-id")
        .unwrap();
    assert_eq!(
        runtime
            .fence_absent_agent_reservation("stable-id")
            .await
            .unwrap_err()
            .code,
        "agent_reservation_observed"
    );
    assert_eq!(
        runtime
            .inner
            .startup_store
            .get("stable-id")
            .unwrap()
            .workload_credential,
        credential
    );
    assert!(!runtime.inner.startup_store.is_denied("stable-id").unwrap());
    runtime
        .inner
        .agents
        .write()
        .unwrap()
        .insert("stable-id".into(), stopped_projection);
    assert!(
        !runtime
            .replace_startup("stable-id", replace())
            .await
            .unwrap()
            .enabled,
        "set preserves stopped intent"
    );
    assert_eq!(
        StartupStore::load(saved_path.clone())
            .unwrap()
            .get("stable-id")
            .unwrap()
            .workload_credential,
        credential
    );
    runtime
        .inner
        .startup_store
        .update("stable-id", |entry| entry.spec.kind = "app".into())
        .unwrap();
    assert_eq!(
        runtime
            .replace_startup("stable-id", replace())
            .await
            .unwrap_err()
            .code,
        "app_startup_managed"
    );
    // Managed App updates use the same lock/generation boundary, but have a
    // separate durable Proxy revision. Stopped Apps must remain stopped.
    let stopped_generation = runtime.get_startup("stable-id").unwrap().generation;
    let stopped = runtime
        .update_app_startup("app-update-1", "stable-id", replace(), 1)
        .await
        .unwrap();
    assert!(!stopped.enabled);
    assert_eq!(stopped.generation, stopped_generation);
    assert_eq!(spawns.lock().unwrap().len(), 3);
    runtime
        .inner
        .startup_store
        .update("stable-id", |entry| entry.spec.enabled = true)
        .unwrap();
    let (updated, restarted) = tokio::join!(
        runtime.update_app_startup("app-update-2", "stable-id", replace(), 2),
        runtime.start("app-restart", "stable-id", true),
    );
    updated.unwrap();
    restarted.unwrap();
    assert_eq!(
        runtime.get_startup("stable-id").unwrap().generation,
        stopped_generation + 2
    );
    assert_eq!(spawns.lock().unwrap().len(), 5);
    runtime
        .update_app_startup("duplicate-update", "stable-id", replace(), 2)
        .await
        .unwrap();
    assert_eq!(
        spawns.lock().unwrap().len(),
        5,
        "same revision must not restart twice"
    );
    assert_eq!(
        runtime
            .update_app_startup("stale-update", "stable-id", replace(), 1)
            .await
            .unwrap_err()
            .code,
        "stale_app_startup"
    );
    let mut conflicting = replace();
    conflicting.args.clear();
    assert_eq!(
        runtime
            .update_app_startup("conflicting-update", "stable-id", conflicting, 2)
            .await
            .unwrap_err()
            .code,
        "app_revision_conflict"
    );
    let saved = StartupStore::load(saved_path.clone())
        .unwrap()
        .get("stable-id")
        .unwrap();
    assert_eq!(saved.app_revision, 2);
    assert_eq!(saved.workload_credential, credential);
    for launch in &spawns.lock().unwrap()[3..] {
        assert_eq!(launch.process_id, "stable-id");
        assert_eq!(launch.env["TREER_WORKLOAD_CREDENTIAL"], credential);
        assert_eq!(launch.command, expected_new_command.to_str().unwrap());
    }
    let declaration: CreateAgentRequest = serde_json::from_value(serde_json::json!({
        "kind":"app", "name":"target", "cwd":"repo",
        "args":["new-command", "literal $() ' spaces"], "cols":80, "rows":24
    }))
    .unwrap();
    let before = runtime.get_startup("stable-id").unwrap().generation;
    runtime
        .ensure_app_lifecycle(
            "reconnect-new-revision",
            "stable-id".into(),
            None,
            declaration.clone(),
            true,
            true,
            Some(3),
        )
        .await
        .unwrap();
    assert_eq!(
        runtime.get_startup("stable-id").unwrap().generation,
        before + 1
    );
    runtime
        .ensure_app_lifecycle(
            "reconnect-duplicate",
            "stable-id".into(),
            None,
            declaration.clone(),
            true,
            true,
            Some(3),
        )
        .await
        .unwrap();
    assert_eq!(
        runtime.get_startup("stable-id").unwrap().generation,
        before + 1
    );
    assert_eq!(
        runtime
            .ensure_app_lifecycle(
                "reconnect-stale",
                "stable-id".into(),
                None,
                declaration,
                true,
                true,
                Some(2)
            )
            .await
            .unwrap_err()
            .code,
        "stale_app_startup"
    );
    let mut broken = replace();
    broken.command = "fail-command".into();
    assert_eq!(
        runtime
            .update_app_startup("failed-app-replacement", "stable-id", broken, 4)
            .await
            .unwrap_err()
            .code,
        "host_error"
    );
    let saved = StartupStore::load(saved_path.clone())
        .unwrap()
        .get("stable-id")
        .unwrap();
    assert_eq!(saved.spec.command, "fail-command");
    assert_eq!(saved.app_revision, 4);
    assert_eq!(saved.workload_credential, credential);
    runtime
        .update_app_startup("repair-app-replacement", "stable-id", replace(), 5)
        .await
        .unwrap();
    assert_eq!(runtime.list()[0].agent_id, "stable-id");
    // #150: an App whose first launch failed before the Host spawned anything
    // keeps only its saved declaration. Stopping it is idempotent (reported as
    // exited from that declaration) and never asks the Host to stop a process
    // it never had; an id with no declaration is simply absent.
    let never_spawned: CreateAgentRequest = serde_json::from_value(serde_json::json!({
        "kind":"app", "name":"never", "cwd":"repo",
        "args":["fail-command"], "cols":80, "rows":24
    }))
    .unwrap();
    let failed = runtime
        .ensure_app_lifecycle(
            "never-spawned-launch",
            "never-spawned".into(),
            Some(credential.clone()),
            never_spawned,
            true,
            true,
            Some(1),
        )
        .await
        .unwrap_err();
    assert_eq!(failed.code, "host_error");
    assert!(runtime.get("never-spawned").is_err(), "no live record");
    let stopped = runtime
        .stop("never-spawned-stop", "never-spawned")
        .await
        .unwrap();
    assert_eq!(stopped.agent_id, "never-spawned");
    assert!(stopped.status.is_terminal());
    assert!(!runtime.get_startup("never-spawned").unwrap().enabled);
    assert_eq!(
        runtime
            .stop("unknown-stop", "no-such-workload")
            .await
            .unwrap_err()
            .code,
        "agent_not_found"
    );
    runtime.inner.workload_proxies.remove("never-spawned");
    task.abort();
    runtime.inner.workload_proxies.remove("stable-id");
    drop(runtime);
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "requires cargo build -p treer-agent-host immediately before this isolated real-Host test"]
async fn recovery_real_host_cold_restart_preserves_intent_identity_and_argv() {
    use sha2::Digest;
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
                protected_paths: vec![],
                workspace_id: "cold-workspace".into(),
                server_id: "cold-server".into(),
                agent_server_url: "http://127.0.0.1:1".into(),
                proxy_public_url: "https://proxy.example/".into(),
                treer_binary: None,
                interface_cache_path: root.join("interfaces.json"),
                startup_store_path: root.join("startup.json"),
                private_state_root: root.join("agent-private"),
                provider_cli_root: root.join("provider-cli"),
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
                .env("TREER_MACHINE_TOKEN", "fixture-not-a-token")
                .env("TREER_OPERATOR_CREDENTIAL", "fixture-not-an-operator")
                .env("TREER_ENROLLMENT_KEY", "fixture-not-an-enrollment")
                .env("AGENT_UI_INSTANCE_ID", "fixture-stale-caller")
                .env("TREER_AGENT_ID", "fixture-stale-agent")
                .env("TREER_WORKLOAD_CREDENTIAL", "fixture-stale-workload")
                .env("HTTP_PROXY", "http://127.0.0.1:1")
                .env("HTTPS_PROXY", "http://127.0.0.1:1")
                .env("ALL_PROXY", "http://127.0.0.1:1")
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
        "test \"$TREER_AGENT_ID\" = \"$2\" && test \"$TREER_WORKSPACE_ID\" = cold-workspace && test \"$TREER_SERVER_ID\" = cold-server && test -n \"$TREER_WORKLOAD_CREDENTIAL\" && test -z \"${TREER_MACHINE_TOKEN+x}${TREER_OPERATOR_CREDENTIAL+x}${TREER_ENROLLMENT_KEY+x}${AGENT_UI_INSTANCE_ID+x}${ALL_PROXY+x}\" && test \"$HTTP_PROXY\" = \"$TREER_HTTP_PROXY\" && test \"$HTTPS_PROXY\" = \"$TREER_HTTP_PROXY\" && test \"$HTTP_PROXY\" != http://127.0.0.1:1 || exit 91; printf '%s\\n' \"$1\"; printf 'identity-ok:%s\\n' \"$TREER_AGENT_ID\"; printf '%s' \"$TREER_WORKLOAD_CREDENTIAL\" | openssl dgst -sha256; exec sleep 120".into(),
        "launcher".into(),
        literal.into(),
    ];
    for id in ["survivor", "stopped", "revoked", "cold-app"] {
        let mut process_args = args.clone();
        process_args.push(id.into());
        first
            .create(
                &format!("create-{id}"),
                id.into(),
                new_workload_credential(),
                CreateAgentRequest {
                    folder_id: None,
                    server_id: None,
                    kind: if id == "cold-app" { "app" } else { "command" }.into(),
                    name: id.into(),
                    cwd: ".".into(),
                    args: process_args,
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
    assert_eq!(before.args[..args.len() - 1], args[1..]);
    let before_credential = first
        .inner
        .startup_store
        .get("survivor")
        .unwrap()
        .workload_credential;
    first.stop("stop-intent", "stopped").await.unwrap();
    assert!(!first.get_startup("stopped").unwrap().enabled);
    shutdown(&mut first_host).await;
    first.inner.workload_proxies.remove("survivor");
    first.inner.workload_proxies.remove("stopped");
    first.inner.workload_proxies.remove("revoked");
    first.inner.workload_proxies.remove("cold-app");
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
    let active = vec![
        "survivor".to_string(),
        "stopped".to_string(),
        "cold-app".to_string(),
    ];
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
    assert!(a
        .items
        .iter()
        .any(|item| item.workload_id == "revoked"
            && item.state == RuntimeRestoreState::IdentityRevoked));
    assert!(a
        .items
        .iter()
        .any(|item| item.workload_id == "cold-app" && item.state == RuntimeRestoreState::Started));
    assert!(
        second
            .inner
            .startup_store
            .get("survivor")
            .unwrap()
            .workload_credential
            == before_credential,
        "cold restoration preserves the stable workload credential"
    );
    let HostResponse::Synced { processes, .. } =
        second.inner.host.sync(Default::default()).await.unwrap()
    else {
        panic!("inventory");
    };
    assert_eq!(
        processes.iter().filter(|process| process.running).count(),
        2
    );
    assert!(processes
        .iter()
        .filter(|process| process.running)
        .all(|process| matches!(process.process_id.as_str(), "survivor" | "cold-app")));
    let deadline = Instant::now() + Duration::from_secs(5);
    for restored_id in ["survivor", "cold-app"] {
        // Only fixture-created credentials are hashed; no installed credentials
        // are inspected and no credential is included in argv or child output.
        let saved = second.inner.startup_store.get(restored_id).unwrap();
        let expected_digest = format!(
            "{:x}",
            sha2::Sha256::digest(saved.workload_credential.as_bytes())
        );
        loop {
            let HostResponse::Output { replay } = second
                .inner
                .host
                .request(
                    HostCommand::Read {
                        process_id: restored_id.into(),
                        cursor: None,
                    },
                    None,
                )
                .await
                .unwrap()
            else {
                panic!("read");
            };
            let text = plain_text(&replay).unwrap();
            if text.contains(literal)
                && text.contains(&format!("identity-ok:{restored_id}"))
                && text.contains(&expected_digest)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "literal argv did not reach restored process"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
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
    let mut stale = processes
        .iter()
        .find(|process| process.process_id == "survivor")
        .unwrap()
        .clone();
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
    second.inner.workload_proxies.remove("cold-app");
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
        folder_id: None,
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
                folder_id: None,
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
                folder_id: None,
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
                folder_id: None,
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
    let credential = new_workload_credential();
    let shell = hot
        .create(
            "admin-shell-create",
            "admin-shell".into(),
            credential.clone(),
            CreateAgentRequest {
                folder_id: None,
                server_id: None,
                kind: "shell".into(),
                name: "temporary admin shell".into(),
                cwd: ".".into(),
                args: vec![],
                cols: 80,
                rows: 24,
                publish_ports: vec![],
                recipe: None,
            },
        )
        .await
        .unwrap();
    let replacement = ReplaceAgentStartupRequest {
        cwd: None,
        command: "/bin/sh".into(),
        args: vec![
            "-c".into(),
            "printf new-command > admin-marker; exec sleep 120".into(),
        ],
    };
    hot.replace_startup("admin-shell", replacement)
        .await
        .unwrap();
    assert!(
        !root.join("admin-marker").exists(),
        "saving does not run the command"
    );
    let restarted = hot
        .start("admin-shell-restart", "admin-shell", true)
        .await
        .unwrap();
    assert_eq!(restarted.agent_id, shell.agent_id);
    assert_eq!(restarted.run_generation, shell.run_generation + 1);
    assert_eq!(
        hot.inner
            .startup_store
            .get("admin-shell")
            .unwrap()
            .workload_credential,
        credential
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !root.join("admin-marker").exists() {
        assert!(
            Instant::now() < deadline,
            "replacement command did not execute on real Host"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        std::fs::read_to_string(root.join("admin-marker")).unwrap(),
        "new-command"
    );
    hot.stop("admin-shell-delete", "admin-shell").await.unwrap();
    hot.clear_startup("admin-shell").unwrap();
    hot.inner.agents.write().unwrap().remove("admin-shell");
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
    // The mock Host does not execute it, but the Controller now requires a
    // platform-valid absolute command before sending a spawn.
    let executable = std::env::current_exe()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let host_executable = executable.clone();
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
    // Host timestamps are wall-clock values encoded at millisecond precision.
    // A fresh spawn can compare <= the restore request (same millisecond or
    // clock adjustment). Keep all generations at a fixed earlier time so the
    // report must describe the actual branch, rather than infer it from time.
    let host_started_at = Utc::now() - chrono::Duration::days(1);
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
                    assert_eq!(request.command, host_executable);
                    assert_eq!(request.cols, 80);
                    assert_eq!(request.rows, 24);
                    assert_eq!(
                        request.env.get("TREER_MACHINE_TOKEN").map(String::as_str),
                        None
                    );
                    assert_eq!(
                        request.env.get("TREER_AGENT_ID").map(String::as_str),
                        Some("recover")
                    );
                    assert_eq!(
                        request.env.get("TREER_WORKSPACE_ID").map(String::as_str),
                        Some("workspace")
                    );
                    assert_eq!(
                        request.env.get("TREER_SERVER_ID").map(String::as_str),
                        Some("server")
                    );
                    assert!(request
                        .env
                        .get("TREER_WORKLOAD_CREDENTIAL")
                        .is_some_and(|value| valid_workload_credential(value)));
                    assert_eq!(
                        request
                            .env
                            .get("TREER_OPERATOR_CREDENTIAL")
                            .map(String::as_str),
                        None
                    );
                    assert!(!request.env.contains_key("TREER_ENROLLMENT_KEY"));
                    host_spawn_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let now = host_started_at;
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
            protected_paths: vec![],
            workspace_id: "workspace".into(),
            server_id: "server".into(),
            agent_server_url: "http://127.0.0.1:1".into(),
            proxy_public_url: "https://proxy.example/".into(),
            treer_binary: None,
            interface_cache_path: root.join("interfaces.json"),
            startup_store_path: root.join("startup.json"),
            private_state_root: root.join("agent-private"),
            provider_cli_root: root.join("provider-cli"),
            root: std::fs::canonicalize(&root).unwrap(),
        },
    )
    .unwrap();
    for (id, enabled) in [("stopped", false), ("revoked", true), ("recover", true)] {
        runtime
            .inner
            .startup_store
            .upsert(StoredAgentStartup {
                app_revision: 0,
                spec: AgentStartupSpec {
                    agent_id: id.into(),
                    server_id: "server".into(),
                    kind: "command".into(),
                    name: id.into(),
                    cwd: ".".into(),
                    command: executable.clone(),
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
    assert_eq!(second.items[0].state, RuntimeRestoreState::Attached);
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
        .await
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
    assert!(crate::machine_upload::validate_file_name("notes.txt").is_ok());
    for name in [
        "",
        ".",
        "..",
        "../notes.txt",
        "dir/notes.txt",
        "dir\\notes.txt",
    ] {
        assert!(
            crate::machine_upload::validate_file_name(name).is_err(),
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
        folder_id: None,
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
        folder_id: None,
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
        folder_id: None,
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
        folder_id: None,
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
        folder_id: None,
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
        folder_id: None,
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

#[test]
fn host_stop_errors_keep_absent_target_codes() {
    use super::output::host_stop_error;
    assert_eq!(
        host_stop_error("process_not_found: process app_1 was not found").code,
        "process_not_found"
    );
    assert_eq!(
        host_stop_error("process_not_running: process app_1 is not running").code,
        "process_not_running"
    );
    for other in [
        "process_not_found_extra: x",
        "spawn_failed: process_not_found",
        "host request timed out",
    ] {
        assert_eq!(host_stop_error(other).code, "host_error", "{other}");
    }
}

/// A runtime over a fake Host that answers every request with an empty,
/// synced inventory: enough for the upload staging paths, which only touch
/// the machine root (#166).
async fn upload_fixture() -> (ControllerRuntime, PathBuf, tokio::task::JoinHandle<()>) {
    use tokio::io::AsyncWriteExt;
    use treer_host_protocol::{
        decode_message, encode_message, HostBuildInfo, HostMessage, HostRequest,
    };
    let temp = std::env::temp_dir().join(format!("trr-upload-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&temp).unwrap();
    // The socket keeps the short, non-canonical path (sockaddr_un length);
    // the machine root is compared canonically (macOS temp dirs live under
    // /private), so the configured root must be canonical.
    #[cfg(unix)]
    let socket = temp.join("host.sock");
    let root = std::fs::canonicalize(&temp).unwrap();
    #[cfg(windows)]
    let socket = treer_platform::endpoint_name(
        &Uuid::new_v4().to_string(),
        &treer_platform::current_user_sid().unwrap(),
    );
    let mut listener = treer_platform::LocalListener::bind(&socket).unwrap();
    let initial = HostResponse::Synced {
        host_epoch: "upload-host".into(),
        host_build: HostBuildInfo {
            version: "test".into(),
            git_commit: "test".into(),
        },
        processes: vec![],
        replay: vec![],
    };
    let inventory = initial.clone();
    let task = tokio::spawn(async move {
        let mut stream = listener.accept().await.unwrap();
        while let Ok(length) = stream.read_u32().await {
            let mut bytes = vec![0; length as usize];
            stream.read_exact(&mut bytes).await.unwrap();
            let request: HostRequest = decode_message(&bytes).unwrap();
            let encoded = encode_message(&HostMessage::Response {
                request_id: request.request_id,
                response: Some(inventory.clone()),
                error: None,
            })
            .unwrap();
            stream.write_u32(encoded.len() as u32).await.unwrap();
            stream.write_all(&encoded).await.unwrap();
        }
    });
    let (host, events) = HostClient::connect(&socket).await.unwrap();
    let config = ControllerConfig {
        protected_paths: vec![],
        workspace_id: "workspace".into(),
        server_id: "server".into(),
        agent_server_url: "http://127.0.0.1:1".into(),
        proxy_public_url: "https://proxy.example/".into(),
        treer_binary: None,
        interface_cache_path: root.join("interfaces.json"),
        startup_store_path: root.join("startup.json"),
        private_state_root: root.join("agent-private"),
        provider_cli_root: root.join("provider-cli"),
        root: root.clone(),
    };
    let (runtime, _) = ControllerRuntime::from_sync(host, initial, events, config).unwrap();
    (runtime, root, task)
}

fn b64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn sha(bytes: &[u8]) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(bytes))
}

fn staging_files(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".treer-transfer-upl_"))
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn chunked_upload_commits_only_verified_parts_and_cleans_staging() {
    let (runtime, root, host_task) = upload_fixture().await;
    std::fs::create_dir_all(root.join("inbox")).unwrap();
    let inbox = root.join("inbox");
    // Over the cap: refused before any staging file exists.
    let error = runtime
        .begin_upload(
            "upl_cap00001",
            "inbox",
            "huge.bin",
            false,
            Some(treer_protocol::MACHINE_UPLOAD_MAX_BYTES + 1),
        )
        .unwrap_err();
    assert_eq!(error.code, "machine_upload_too_large");
    assert!(staging_files(&inbox).is_empty());
    // A six-byte upload in two parts, with one bad part in between.
    runtime
        .begin_upload("upl_ok000001", "inbox", "ok.bin", false, Some(6))
        .unwrap();
    assert_eq!(
        staging_files(&inbox),
        vec![".treer-transfer-upl_ok000001".to_string()]
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(inbox.join(".treer-transfer-upl_ok000001"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "staging file is owner-only");
    }
    assert_eq!(
        runtime
            .append_upload_chunk("upl_ok000001", None, &b64(b"abc"))
            .unwrap_err()
            .code,
        "machine_upload_mode_mismatch",
        "declared-size uploads need offsets"
    );
    assert_eq!(
        runtime
            .append_upload_chunk("upl_ok000001", Some(0), &b64(b"abc"))
            .unwrap(),
        3
    );
    let committed = runtime
        .commit_upload_part("upl_ok000001", 0, 3, &sha(b"abc"))
        .unwrap();
    assert_eq!(committed["committed_offset"], 3);
    assert_eq!(
        runtime
            .append_upload_chunk("upl_ok000001", Some(3), &b64(b"de"))
            .unwrap(),
        5
    );
    let error = runtime
        .commit_upload_part("upl_ok000001", 3, 2, &sha(b"xx"))
        .unwrap_err();
    assert_eq!(error.code, "upload_part_hash_mismatch");
    let state = runtime.upload_result("upl_ok000001").unwrap();
    assert_eq!(state["state"], "active");
    assert_eq!(state["committed_offset"], 3);
    assert_eq!(state["written_offset"], 3, "the bad part was discarded");
    assert_eq!(
        runtime
            .append_upload_chunk("upl_ok000001", Some(2), &b64(b"d"))
            .unwrap_err()
            .code,
        "machine_upload_offset"
    );
    assert_eq!(
        runtime
            .append_upload_chunk("upl_ok000001", Some(3), &b64(b"def"))
            .unwrap(),
        6
    );
    assert_eq!(
        runtime
            .commit_upload_part("upl_ok000001", 3, 3, &sha(b"def"))
            .unwrap()["committed_offset"],
        6
    );
    // Idempotent and conflicting replays of a committed part.
    assert_eq!(
        runtime
            .commit_upload_part("upl_ok000001", 0, 3, &sha(b"abc"))
            .unwrap()["idempotent"],
        true
    );
    assert_eq!(
        runtime
            .commit_upload_part("upl_ok000001", 0, 3, &sha(b"zzz"))
            .unwrap_err()
            .code,
        "upload_part_conflict"
    );
    let receipt = runtime.commit_upload("upl_ok000001").unwrap();
    assert_eq!(receipt.bytes_written, 6);
    assert_eq!(receipt.path, "inbox/ok.bin");
    assert_eq!(receipt.sha256.as_deref(), Some(sha(b"abcdef").as_str()));
    assert_eq!(std::fs::read(inbox.join("ok.bin")).unwrap(), b"abcdef");
    assert!(staging_files(&inbox).is_empty());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(inbox.join("ok.bin"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // The receipt is remembered: a repeated commit and the result query agree.
    assert_eq!(runtime.commit_upload("upl_ok000001").unwrap(), receipt);
    let state = runtime.upload_result("upl_ok000001").unwrap();
    assert_eq!(state["state"], "committed");
    assert_eq!(state["result"]["sha256"], sha(b"abcdef"));
    assert!(!runtime.abort_upload("upl_ok000001").unwrap());
    // Incomplete uploads cannot be committed; abort removes the staging file.
    runtime
        .begin_upload("upl_short0001", "inbox", "short.bin", false, Some(5))
        .unwrap();
    runtime
        .append_upload_chunk("upl_short0001", Some(0), &b64(b"abc"))
        .unwrap();
    runtime
        .commit_upload_part("upl_short0001", 0, 3, &sha(b"abc"))
        .unwrap();
    assert_eq!(
        runtime.commit_upload("upl_short0001").unwrap_err().code,
        "machine_upload_size_mismatch"
    );
    assert!(runtime.abort_upload("upl_short0001").unwrap());
    assert!(staging_files(&inbox).is_empty());
    assert!(!inbox.join("short.bin").exists());
    // A chunk past the declaration is discarded and the part rewinds.
    runtime
        .begin_upload("upl_long00001", "inbox", "long.bin", false, Some(2))
        .unwrap();
    assert_eq!(
        runtime
            .append_upload_chunk("upl_long00001", Some(0), &b64(b"abc"))
            .unwrap_err()
            .code,
        "machine_upload_too_large"
    );
    assert_eq!(
        runtime.upload_result("upl_long00001").unwrap()["written_offset"],
        0
    );
    assert!(runtime.abort_upload("upl_long00001").unwrap());
    // Publishing never replaces an existing file unless asked.
    std::fs::write(inbox.join("taken.bin"), b"old").unwrap();
    for (id, overwrite) in [("upl_taken0001", false), ("upl_taken0002", true)] {
        runtime
            .begin_upload(id, "inbox", "taken.bin", overwrite, Some(3))
            .unwrap();
        runtime
            .append_upload_chunk(id, Some(0), &b64(b"new"))
            .unwrap();
        runtime.commit_upload_part(id, 0, 3, &sha(b"new")).unwrap();
        let outcome = runtime.commit_upload(id);
        if overwrite {
            assert_eq!(outcome.unwrap().bytes_written, 3);
            assert_eq!(std::fs::read(inbox.join("taken.bin")).unwrap(), b"new");
        } else {
            assert_eq!(outcome.unwrap_err().code, "machine_file_exists");
            assert_eq!(std::fs::read(inbox.join("taken.bin")).unwrap(), b"old");
        }
        assert!(staging_files(&inbox).is_empty());
    }
    // Legacy (undeclared) uploads append and publish under the cap.
    runtime
        .begin_upload("upl_legacy001", "inbox", "legacy.bin", false, None)
        .unwrap();
    assert_eq!(
        runtime
            .append_upload_chunk("upl_legacy001", Some(0), &b64(b"xy"))
            .unwrap_err()
            .code,
        "machine_upload_mode_mismatch"
    );
    runtime
        .append_upload_chunk("upl_legacy001", None, &b64(b"xy"))
        .unwrap();
    let legacy = runtime.commit_upload("upl_legacy001").unwrap();
    assert_eq!(legacy.bytes_written, 2);
    assert_eq!(legacy.sha256.as_deref(), Some(sha(b"xy").as_str()));
    host_task.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn chunked_upload_bounds_active_uploads_reaps_idle_ones_and_sweeps_after_restart() {
    let (runtime, root, host_task) = upload_fixture().await;
    for index in 0..treer_protocol::MACHINE_UPLOAD_MAX_ACTIVE {
        runtime
            .begin_upload(
                &format!("upl_active{index:03}"),
                ".",
                &format!("file-{index}.bin"),
                false,
                Some(1024),
            )
            .unwrap();
    }
    let error = runtime
        .begin_upload("upl_extra0001", ".", "extra.bin", false, Some(1024))
        .unwrap_err();
    assert_eq!(error.code, "machine_upload_busy");
    assert_eq!(
        staging_files(&root).len(),
        treer_protocol::MACHINE_UPLOAD_MAX_ACTIVE
    );
    assert_eq!(runtime.reap_uploads(), 0);
    runtime.inner.uploads.age_for_test(
        "upl_active000",
        Duration::from_secs(treer_protocol::MACHINE_UPLOAD_IDLE_SECONDS + 1),
        Duration::from_secs(0),
    );
    runtime.inner.uploads.age_for_test(
        "upl_active001",
        Duration::from_secs(0),
        Duration::from_secs(treer_protocol::MACHINE_UPLOAD_MAX_LIFETIME_SECONDS + 1),
    );
    assert_eq!(runtime.reap_uploads(), 2);
    let remaining = staging_files(&root);
    assert_eq!(
        remaining.len(),
        treer_protocol::MACHINE_UPLOAD_MAX_ACTIVE - 2
    );
    assert!(!remaining.contains(&".treer-transfer-upl_active000".to_string()));
    assert_eq!(
        runtime
            .append_upload_chunk("upl_active000", Some(0), &b64(b"late"))
            .unwrap_err()
            .code,
        "machine_upload_not_found"
    );
    runtime
        .begin_upload("upl_extra0001", ".", "extra.bin", false, Some(1024))
        .unwrap();
    assert_eq!(
        staging_files(&root).len(),
        treer_protocol::MACHINE_UPLOAD_MAX_ACTIVE - 1
    );
    // A replacement Controller sweeps exactly the staging names its
    // predecessor recorded, and nothing else. Here the predecessor is still
    // alive and holds its staging files open: on Windows the sweep cannot
    // delete them (sharing mode), so begin must still succeed, the held names
    // stay recorded for the next sweep, and nothing else is touched.
    let recorded_before = staging_files(&root).len();
    std::fs::write(root.join(".treer-transfer-upl_unrecorded"), b"not ours").unwrap();
    let replacement = crate::machine_upload::Uploads::new(root.clone(), "server".into(), vec![]);
    replacement
        .begin("upl_fresh00001", ".", "fresh.bin", false, Some(8))
        .unwrap();
    let after = staging_files(&root);
    assert!(after.contains(&".treer-transfer-upl_fresh00001".to_string()));
    assert!(after.contains(&".treer-transfer-upl_unrecorded".to_string()));
    if cfg!(windows) {
        assert_eq!(
            replacement.unswept_for_test().len(),
            recorded_before,
            "held-open names are kept for the next sweep"
        );
        assert_eq!(after.len(), recorded_before + 2);
    } else {
        assert!(replacement.unswept_for_test().is_empty());
        assert_eq!(
            after,
            vec![
                ".treer-transfer-upl_fresh00001".to_string(),
                ".treer-transfer-upl_unrecorded".to_string()
            ],
            "recorded intents swept, unrecorded names untouched"
        );
    }
    assert!(replacement.abort("upl_fresh00001").unwrap());
    std::fs::remove_file(root.join(".treer-transfer-upl_unrecorded")).unwrap();
    host_task.abort();
    // The test leaves seven uploads active on purpose, so their staging
    // handles stay open inside the predecessor's registry; on Windows the
    // directory cannot be removed while they are. Abort them through the
    // Controller API, which closes and removes each staging file
    // deterministically, before dropping both registries.
    drop(replacement);
    for index in 2..treer_protocol::MACHINE_UPLOAD_MAX_ACTIVE {
        assert!(runtime
            .abort_upload(&format!("upl_active{index:03}"))
            .unwrap());
    }
    assert!(runtime.abort_upload("upl_extra0001").unwrap());
    drop(runtime);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn chunked_upload_commit_that_provably_did_not_publish_leaves_a_definitive_failure_receipt() {
    let (runtime, root, host_task) = upload_fixture().await;
    runtime
        .begin_upload("upl_failrcpt01", ".", "failrcpt.bin", false, Some(3))
        .unwrap();
    runtime
        .append_upload_chunk("upl_failrcpt01", Some(0), &b64(b"abc"))
        .unwrap();
    runtime
        .commit_upload_part("upl_failrcpt01", 0, 3, &sha(b"abc"))
        .unwrap();
    let state = runtime.upload_result("upl_failrcpt01").unwrap();
    assert_eq!(state["state"], "active");
    // A directory now sits where the file would be published: the no-replace
    // publish cannot succeed and nothing is linked or renamed.
    std::fs::create_dir(root.join("failrcpt.bin")).unwrap();
    let first = runtime.commit_upload("upl_failrcpt01").unwrap_err();
    assert!(!first.code.is_empty() && first.code != "machine_upload_commit_uncertain");
    // First terminal wins: the replayed commit answers the same failure, the
    // registry no longer holds the upload, and UploadResult says failed.
    let again = runtime.commit_upload("upl_failrcpt01").unwrap_err();
    assert_eq!(again.code, first.code);
    let result = runtime.upload_result("upl_failrcpt01").unwrap();
    assert_eq!(result["state"], "failed", "{result}");
    assert_eq!(result["code"], first.code);
    assert!(
        staging_files(&root).is_empty(),
        "a failed publish drops its staging file"
    );
    // The receipt is journaled: a replacement Controller answers it too.
    let replacement = crate::machine_upload::Uploads::new(root.clone(), "server".into(), vec![]);
    let replayed = replacement.result("upl_failrcpt01").unwrap();
    assert_eq!(replayed["state"], "failed");
    assert_eq!(replayed["code"], first.code);
    // A later successful upload under another id is unaffected.
    runtime
        .begin_upload("upl_failrcpt02", ".", "ok.bin", false, Some(2))
        .unwrap();
    runtime
        .append_upload_chunk("upl_failrcpt02", Some(0), &b64(b"ok"))
        .unwrap();
    runtime
        .commit_upload_part("upl_failrcpt02", 0, 2, &sha(b"ok"))
        .unwrap();
    let ok = runtime.commit_upload("upl_failrcpt02").unwrap();
    assert_eq!(ok.bytes_written, 2);
    std::fs::remove_dir(root.join("failrcpt.bin")).unwrap();
    host_task.abort();
    drop(replacement);
    drop(runtime);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn interface_registration_limiter_refuses_the_eleventh_in_a_window_and_bounds_tracked_agents() {
    let mut limiter = InterfaceRegistrationLimiter::default();
    let start = Instant::now();
    for _ in 0..INTERFACE_REGISTRATIONS_PER_WINDOW {
        limiter.admit("ag_one", start).unwrap();
    }
    // The eleventh is refused; the first refusal in the window is reported
    // once, later ones silently.
    assert_eq!(limiter.admit("ag_one", start), Err(true));
    assert_eq!(limiter.admit("ag_one", start), Err(false));
    // Another Agent's budget is independent.
    limiter.admit("ag_two", start).unwrap();
    // After the window the Agent registers again.
    let later = start + INTERFACE_REGISTRATION_WINDOW + Duration::from_secs(1);
    limiter.admit("ag_one", later).unwrap();
    // The tracking map is capped: the oldest window is evicted, never grown.
    let mut limiter = InterfaceRegistrationLimiter::default();
    for index in 0..INTERFACE_REGISTRATION_TRACKED_AGENTS {
        limiter
            .admit(
                &format!("ag_{index}"),
                start + Duration::from_millis(index as u64),
            )
            .unwrap();
    }
    assert_eq!(limiter.windows.len(), INTERFACE_REGISTRATION_TRACKED_AGENTS);
    limiter
        .admit("ag_overflow", start + Duration::from_secs(1))
        .unwrap();
    assert_eq!(limiter.windows.len(), INTERFACE_REGISTRATION_TRACKED_AGENTS);
    assert!(
        !limiter.windows.contains_key("ag_0"),
        "the oldest window was evicted"
    );
    assert!(limiter.windows.contains_key("ag_overflow"));
}

#[cfg(unix)]
#[tokio::test]
async fn startup_sweep_counts_consecutive_failures_per_name_and_gives_up_after_five() {
    use std::os::unix::fs::PermissionsExt;
    let (runtime, root, host_task) = upload_fixture().await;
    std::fs::create_dir(root.join("locked")).unwrap();
    std::fs::create_dir(root.join("gone")).unwrap();
    runtime
        .begin_upload("upl_sweepfail01", "locked", "a.bin", false, Some(8))
        .unwrap();
    runtime
        .begin_upload("upl_sweepgone01", "gone", "b.bin", false, Some(8))
        .unwrap();
    // Both intents are journaled; the Controller "crashes" with the staging
    // files in place.
    host_task.abort();
    std::mem::forget(runtime);
    // One parent is now unwritable (the unlink fails with EACCES), the other
    // is gone entirely (nothing left to sweep).
    std::fs::set_permissions(root.join("locked"), std::fs::Permissions::from_mode(0o500)).unwrap();
    std::fs::remove_dir_all(root.join("gone")).unwrap();
    let mut remembered = Vec::new();
    for attempt in 1..=5 {
        let replacement =
            crate::machine_upload::Uploads::new(root.clone(), "server".into(), vec![]);
        replacement.recover_at_startup();
        let unswept = replacement.unswept_for_test();
        remembered.push(unswept.len());
        if attempt < 5 {
            assert_eq!(
                unswept,
                vec![("locked/a.bin".to_string(), "upl_sweepfail01".to_string())],
                "attempt {attempt}: the unwritable name is kept, the missing directory is not"
            );
        } else {
            assert!(
                unswept.is_empty(),
                "the fifth failed sweep drops the record"
            );
        }
        drop(replacement);
    }
    assert_eq!(remembered, vec![1, 1, 1, 1, 0]);
    // The file itself is left for the operator.
    std::fs::set_permissions(root.join("locked"), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(root.join("locked/.treer-transfer-upl_sweepfail01").exists());
    // A later start no longer touches it: the journal has no record.
    let fresh = crate::machine_upload::Uploads::new(root.clone(), "server".into(), vec![]);
    fresh.recover_at_startup();
    assert!(fresh.unswept_for_test().is_empty());
    assert!(root.join("locked/.treer-transfer-upl_sweepfail01").exists());
    drop(fresh);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn chunked_upload_rewinds_short_writes_and_refuses_to_publish_tampered_staging() {
    let (runtime, root, host_task) = upload_fixture().await;
    // The first chunk of a part fails half-way: nothing of it stays, the
    // retry lands cleanly and the published file matches the receipt hash.
    runtime
        .begin_upload("upl_short0002", ".", "short.bin", false, Some(6))
        .unwrap();
    *crate::machine_upload::FAIL_NEXT_WRITE_FOR.lock().unwrap() = Some("upl_short0002".into());
    let error = runtime
        .append_upload_chunk("upl_short0002", Some(0), &b64(b"abc"))
        .unwrap_err();
    assert_eq!(error.code, "machine_upload_failed");
    let state = runtime.upload_result("upl_short0002").unwrap();
    assert_eq!(state["written_offset"], 0);
    assert_eq!(
        std::fs::metadata(root.join(".treer-transfer-upl_short0002"))
            .unwrap()
            .len(),
        0,
        "partial bytes are discarded"
    );
    runtime
        .append_upload_chunk("upl_short0002", Some(0), &b64(b"abc"))
        .unwrap();
    runtime
        .commit_upload_part("upl_short0002", 0, 3, &sha(b"abc"))
        .unwrap();
    runtime
        .append_upload_chunk("upl_short0002", Some(3), &b64(b"def"))
        .unwrap();
    runtime
        .commit_upload_part("upl_short0002", 3, 3, &sha(b"def"))
        .unwrap();
    let receipt = runtime.commit_upload("upl_short0002").unwrap();
    assert_eq!(std::fs::read(root.join("short.bin")).unwrap(), b"abcdef");
    assert_eq!(receipt.sha256.as_deref(), Some(sha(b"abcdef").as_str()));
    // Staging tampered between the last part and the commit: the commit
    // fails closed, the staging stays for abort, nothing is published.
    runtime
        .begin_upload("upl_tamper001", ".", "tamper.bin", false, Some(3))
        .unwrap();
    runtime
        .append_upload_chunk("upl_tamper001", Some(0), &b64(b"abc"))
        .unwrap();
    runtime
        .commit_upload_part("upl_tamper001", 0, 3, &sha(b"abc"))
        .unwrap();
    // Through the Controller's own handle: production never writes the file
    // from outside, and Windows refuses an external writer by sharing mode.
    runtime
        .inner
        .uploads
        .write_staging_for_test("upl_tamper001", 0, b"abX");
    let error = runtime.commit_upload("upl_tamper001").unwrap_err();
    assert_eq!(error.code, "machine_upload_content_mismatch");
    assert!(!root.join("tamper.bin").exists());
    assert!(root.join(".treer-transfer-upl_tamper001").exists());
    assert_eq!(
        runtime.upload_result("upl_tamper001").unwrap()["state"],
        "active"
    );
    assert!(runtime.abort_upload("upl_tamper001").unwrap());
    assert!(!root.join(".treer-transfer-upl_tamper001").exists());
    // A part whose bytes on disk differ in length from the acknowledged
    // count is discarded at part commit.
    runtime
        .begin_upload("upl_length001", ".", "length.bin", false, Some(4))
        .unwrap();
    runtime
        .append_upload_chunk("upl_length001", Some(0), &b64(b"ab"))
        .unwrap();
    runtime
        .inner
        .uploads
        .write_staging_for_test("upl_length001", 2, b"cd");
    let error = runtime
        .commit_upload_part("upl_length001", 0, 2, &sha(b"ab"))
        .unwrap_err();
    assert_eq!(error.code, "machine_upload_file_mismatch");
    assert_eq!(
        runtime.upload_result("upl_length001").unwrap()["written_offset"],
        0
    );
    assert!(runtime.abort_upload("upl_length001").unwrap());
    host_task.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn chunked_upload_capability_matches_the_dispatch_gate_and_legacy_stays() {
    let (runtime, root, host_task) = upload_fixture().await;
    let advertised = runtime.advertised_capabilities();
    let v3 = advertised.contains(&treer_protocol::CAPABILITY_MACHINE_UPLOAD_V3.to_string());
    assert_eq!(
        v3,
        crate::machine_upload::chunked_upload_supported(),
        "the capability is advertised exactly where chunked uploads are supported"
    );
    assert_eq!(v3, !cfg!(windows), "Windows waits for the #171 validation");
    let begin = |size: Option<u64>| treer_protocol::AgentCommand::UploadBegin {
        upload_id: "upl_gate000001".into(),
        directory: ".".into(),
        file_name: "gate.bin".into(),
        overwrite: false,
        size,
    };
    // The dispatch refuses every v3 command where the capability is not
    // advertised and keeps the legacy commands.
    assert_eq!(
        crate::proxy::command_permitted(&advertised, &begin(Some(8))),
        v3
    );
    assert_eq!(
        crate::proxy::command_permitted(
            &advertised,
            &treer_protocol::AgentCommand::UploadChunk {
                upload_id: "upl_gate000001".into(),
                content_base64: String::new(),
                offset: Some(0),
            }
        ),
        v3
    );
    assert_eq!(
        crate::proxy::command_permitted(
            &advertised,
            &treer_protocol::AgentCommand::UploadPartCommit {
                upload_id: "upl_gate000001".into(),
                offset: 0,
                length: 1,
                sha256: "00".into(),
            }
        ),
        v3
    );
    assert_eq!(
        crate::proxy::command_permitted(
            &advertised,
            &treer_protocol::AgentCommand::UploadResult {
                upload_id: "upl_gate000001".into(),
            }
        ),
        v3
    );
    assert!(crate::proxy::command_permitted(&advertised, &begin(None)));
    assert!(crate::proxy::command_permitted(
        &advertised,
        &treer_protocol::AgentCommand::UploadChunk {
            upload_id: "upl_gate000001".into(),
            content_base64: String::new(),
            offset: None,
        }
    ));
    assert!(crate::proxy::command_permitted(
        &advertised,
        &treer_protocol::AgentCommand::UploadCommit {
            upload_id: "upl_gate000001".into(),
        }
    ));
    // Legacy begin, append and commit work on every platform.
    runtime
        .begin_upload("upl_legacy0002", ".", "legacy2.bin", false, None)
        .unwrap();
    runtime
        .append_upload_chunk("upl_legacy0002", None, &b64(b"xy"))
        .unwrap();
    let legacy = runtime.commit_upload("upl_legacy0002").unwrap();
    assert_eq!(legacy.bytes_written, 2);
    assert_eq!(std::fs::read(root.join("legacy2.bin")).unwrap(), b"xy");
    assert!(staging_files(&root).is_empty());
    host_task.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn receipt_cap_evicts_failed_receipts_before_the_oldest_committed_one() {
    let (runtime, root, host_task) = upload_fixture().await;
    drop(runtime);
    let uploads = crate::machine_upload::Uploads::new(root.clone(), "server".into(), vec![]);
    uploads.recover_at_startup();
    // The oldest receipt is committed, then one failed receipt, then enough
    // committed ones to sit exactly at the cap.
    uploads.seed_receipt_for_test("upl_capoldest01", false);
    uploads.seed_receipt_for_test("upl_capfailed01", true);
    for index in 0..(crate::machine_upload::MAX_STORED_RESULTS - 2) {
        uploads.seed_receipt_for_test(&format!("upl_cap{index:08}"), false);
    }
    assert_eq!(
        uploads.receipts_for_test().len(),
        crate::machine_upload::MAX_STORED_RESULTS
    );
    // A real commit pushes the journal over the cap: the failed receipt goes,
    // not the oldest committed one.
    uploads
        .begin("upl_capnewest01", ".", "newest.bin", false, Some(2))
        .unwrap();
    uploads
        .chunk("upl_capnewest01", Some(0), &b64(b"ok"))
        .unwrap();
    uploads
        .part_commit("upl_capnewest01", 0, 2, &sha(b"ok"))
        .unwrap();
    uploads.commit("upl_capnewest01").unwrap();
    let receipts = uploads.receipts_for_test();
    assert_eq!(receipts.len(), crate::machine_upload::MAX_STORED_RESULTS);
    assert!(receipts.iter().all(|(_, failed)| !failed));
    assert_eq!(receipts.first().unwrap().0, "upl_capoldest01");
    assert_eq!(receipts.last().unwrap().0, "upl_capnewest01");
    // Once no failed receipt is left, the oldest committed one goes.
    uploads.seed_receipt_for_test("upl_capnewest02", false);
    let receipts = uploads.receipts_for_test();
    assert_eq!(receipts.len(), crate::machine_upload::MAX_STORED_RESULTS);
    assert_eq!(receipts.first().unwrap().0, "upl_cap00000000");
    // The journal on disk agrees: a replacement Controller answers the kept
    // receipts and no longer knows the evicted ones.
    let replacement = crate::machine_upload::Uploads::new(root.clone(), "server".into(), vec![]);
    assert_eq!(
        replacement.result("upl_capnewest01").unwrap()["state"],
        "committed"
    );
    assert_eq!(
        replacement.result("upl_capfailed01").unwrap()["state"],
        "unknown"
    );
    assert_eq!(
        replacement.result("upl_capoldest01").unwrap()["state"],
        "unknown"
    );
    host_task.abort();
    drop(uploads);
    drop(replacement);
    std::fs::remove_dir_all(root).unwrap();
}

/// A Controller whose Host inventory holds one running Agent `command`, so
/// `register_interface` passes `get(agent_id)`; nothing else is spawned.
async fn agent_fixture() -> (ControllerRuntime, PathBuf, tokio::task::JoinHandle<()>) {
    agent_fixture_configured(|_| {}).await
}

async fn update_state_fixture() -> (ControllerRuntime, PathBuf, tokio::task::JoinHandle<()>) {
    agent_fixture_configured(|config| {
        config.workspace_id = "ws_fixture".into();
        config.server_id = "srv_fixture".into();
        let state = config.root.join("configured-state");
        std::fs::create_dir(&state).unwrap();
        treer_platform::secure_directory(&config.root).unwrap();
        treer_platform::secure_directory(&state).unwrap();
        config.startup_store_path = state.join("srv_fixture-agent-startup.json");
        StartupStore::load(config.startup_store_path.clone())
            .unwrap()
            .deny_absent("ag_deleted")
            .unwrap();
    })
    .await
}

fn update_state_binding(operation_id: Uuid) -> crate::windows_update_intent::Binding {
    crate::windows_update_intent::Binding {
        operation_id,
        workspace_id: "ws_fixture".into(),
        server_id: "srv_fixture".into(),
        installation_volume: 1,
        installation_file_id: 2,
        old_controller_sha256: "a".repeat(64),
        old_cli_sha256: "b".repeat(64),
        controller_sha256: "c".repeat(64),
        cli_sha256: "d".repeat(64),
        signed_manifest_sha256: "e".repeat(64),
    }
}

#[tokio::test]
async fn windows_update_state_runtime_scope_refuses_foreign_operation_and_machine() {
    let (runtime, root, task) = update_state_fixture().await;
    let operation = Uuid::new_v4();
    for (op, ws, server) in [
        (Uuid::nil(), "ws_fixture", "srv_fixture"),
        (operation, "ws_other", "srv_fixture"),
        (operation, "ws_fixture", "srv_other"),
    ] {
        assert!(runtime
            .windows_update_state_for_operation(op, ws, server)
            .is_err());
    }
    let state = runtime
        .windows_update_state_for_operation(operation, "ws_fixture", "srv_fixture")
        .unwrap();
    let mut binding = update_state_binding(operation);
    state.matches(&binding).unwrap();
    binding.operation_id = Uuid::new_v4();
    assert!(state.matches(&binding).is_err());
    binding.operation_id = operation;
    binding.server_id = "srv_other".into();
    assert!(state.matches(&binding).is_err());
    drop(state);
    drop(runtime);
    task.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn windows_update_state_retains_exact_loaded_store_and_runtime_instance() {
    let (runtime, root, task) = update_state_fixture().await;
    let (other, other_root, other_task) = update_state_fixture().await;
    let operation = Uuid::new_v4();
    let state = runtime
        .windows_update_state_for_operation(operation, "ws_fixture", "srv_fixture")
        .unwrap();
    let next = other
        .windows_update_state_for_operation(operation, "ws_fixture", "srv_fixture")
        .unwrap();
    assert_ne!(state.evidence(), next.evidence());
    let safe = serde_json::to_string(state.evidence()).unwrap();
    assert!(!safe.contains("configured-state"));
    assert!(!safe.contains("agent-host"));
    drop(runtime);
    state.matches(&update_state_binding(operation)).unwrap();
    // The in-process authority retains the SAME runtime even after its caller
    // drops a reference; it is not reconstituted from the safe evidence JSON.
    drop(state);
    drop(next);
    drop(other);
    task.abort();
    other_task.abort();
    std::fs::remove_dir_all(root).unwrap();
    std::fs::remove_dir_all(other_root).unwrap();
}

fn update_custody_command() -> std::process::Command {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .env_clear()
        .args([
            "--exact",
            "controller::tests::update_custody_fixture_child",
            "--ignored",
            "--nocapture",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    command
}

#[test]
#[ignore = "owned helper, only invoked by the custody parent controls"]
fn update_custody_fixture_child() {
    use std::io::Read;
    let mut byte = [0];
    std::io::stdin().read_exact(&mut byte).unwrap();
    std::process::exit(259);
}

#[tokio::test]
async fn windows_update_pipe_scope_precedes_accept_and_exact_owner_is_single_use() {
    let (runtime, root, task) = update_state_fixture().await;
    let (other, other_root, other_task) = update_state_fixture().await;
    let binding = update_state_binding(Uuid::new_v4());
    let state = runtime
        .windows_update_state_for_operation(binding.operation_id, "ws_fixture", "srv_fixture")
        .unwrap();
    let mut child = update_state::ControllerUpdateChild::spawn_fixture(
        &state,
        &runtime,
        &binding,
        &mut update_custody_command(),
    )
    .unwrap();
    assert!(child.begin_fixture_pipe_accept(&other, &binding).is_err());
    let mut foreign = binding.clone();
    foreign.operation_id = Uuid::new_v4();
    assert!(child.begin_fixture_pipe_accept(&runtime, &foreign).is_err());
    child.begin_fixture_pipe_accept(&runtime, &binding).unwrap();
    assert!(child.begin_fixture_pipe_accept(&runtime, &binding).is_err());
    child.stop(&runtime, &binding).unwrap();
    drop(child);
    drop(state);
    drop(runtime);
    drop(other);
    task.abort();
    other_task.abort();
    std::fs::remove_dir_all(root).unwrap();
    std::fs::remove_dir_all(other_root).unwrap();
}

#[tokio::test]
async fn windows_update_pipe_consumed_attempt_survives_interruption_without_consuming_stop() {
    let (runtime, root, task) = update_state_fixture().await;
    let binding = update_state_binding(Uuid::new_v4());
    let state = runtime
        .windows_update_state_for_operation(binding.operation_id, "ws_fixture", "srv_fixture")
        .unwrap();
    let mut child = update_state::ControllerUpdateChild::spawn_fixture(
        &state,
        &runtime,
        &binding,
        &mut update_custody_command(),
    )
    .unwrap();
    // Isolate the synchronous pre-await transition; no wire/OS result can
    // undo it, including cancellation before any connection is observed.
    child.begin_fixture_pipe_accept(&runtime, &binding).unwrap();
    assert!(child.begin_fixture_pipe_accept(&runtime, &binding).is_err());
    assert!(child.poll(&runtime, &binding).unwrap().is_none());
    child.stop(&runtime, &binding).unwrap();
    assert!(child.begin_fixture_pipe_accept(&runtime, &binding).is_err());
    drop(child);
    drop(state);
    drop(runtime);
    task.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(windows)]
#[tokio::test]
#[ignore = "owned pipe helper invoked only by the focused parent control"]
async fn update_pipe_fixture_child() {
    use std::io::Read;
    use tokio::io::AsyncWriteExt;
    let endpoint = std::env::var_os("TREER_TEST_UPDATE_PIPE").unwrap();
    let mut client = tokio::net::windows::named_pipe::ClientOptions::new()
        .open(endpoint)
        .unwrap();
    client.write_all(&[0x51]).await.unwrap();
    let mut byte = [0];
    std::io::stdin().read_exact(&mut byte).unwrap();
}

#[cfg(windows)]
#[tokio::test]
async fn windows_update_pipe_native_original_child_peer_and_exclusive_name() {
    use treer_platform::windows_update_pipe::CreatedChildPipe;
    let (runtime, root, task) = update_state_fixture().await;
    let binding = update_state_binding(Uuid::new_v4());
    let state = runtime
        .windows_update_state_for_operation(binding.operation_id, "ws_fixture", "srv_fixture")
        .unwrap();
    let selector = Uuid::new_v4().simple().to_string();
    let pipe = CreatedChildPipe::bind(&selector).unwrap();
    assert!(CreatedChildPipe::bind(&selector).is_err());
    for invalid in ["", "../pipe", "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"] {
        assert!(CreatedChildPipe::bind(invalid).is_err());
    }
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .env_clear()
        .env("TREER_TEST_UPDATE_PIPE", pipe.endpoint())
        .args([
            "--exact",
            "controller::tests::update_pipe_fixture_child",
            "--ignored",
            "--nocapture",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut child = update_state::ControllerUpdateChild::spawn_fixture(
        &state,
        &runtime,
        &binding,
        &mut command,
    )
    .unwrap();
    child
        .accept_worker_pipe(&runtime, &binding, pipe)
        .await
        .unwrap();
    assert_eq!(child.read_fixture_pipe_byte().await, 0x51);
    child.stop(&runtime, &binding).unwrap();
    drop(child);
    drop(state);
    drop(runtime);
    task.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(windows)]
#[tokio::test]
async fn windows_update_pipe_native_foreign_peer_and_cancelled_accept_do_not_reconnect() {
    use treer_platform::windows_update_pipe::CreatedChildPipe;
    let (runtime, root, task) = update_state_fixture().await;
    let binding = update_state_binding(Uuid::new_v4());
    let state = runtime
        .windows_update_state_for_operation(binding.operation_id, "ws_fixture", "srv_fixture")
        .unwrap();
    for cancelled in [false, true] {
        let pipe = CreatedChildPipe::bind(&Uuid::new_v4().simple().to_string()).unwrap();
        let mut child = update_state::ControllerUpdateChild::spawn_fixture(
            &state,
            &runtime,
            &binding,
            &mut update_custody_command(),
        )
        .unwrap();
        if cancelled {
            assert!(tokio::time::timeout(
                std::time::Duration::from_millis(10),
                child.accept_worker_pipe(&runtime, &binding, pipe)
            )
            .await
            .is_err());
        } else {
            // Parent is a real local peer, but is NOT our created Child.
            let foreign = tokio::net::windows::named_pipe::ClientOptions::new()
                .open(pipe.endpoint())
                .unwrap();
            let error = child
                .accept_worker_pipe(&runtime, &binding, pipe)
                .await
                .unwrap_err();
            assert_eq!(error.to_string(), "windows_update_child_pipe_unconfirmed");
            drop(foreign);
        }
        let replacement = CreatedChildPipe::bind(&Uuid::new_v4().simple().to_string()).unwrap();
        let error = child
            .accept_worker_pipe(&runtime, &binding, replacement)
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "windows_update_child_pipe_already_attempted"
        );
        assert!(child.poll(&runtime, &binding).unwrap().is_none());
        child.stop(&runtime, &binding).unwrap();
        drop(child);
    }
    drop(state);
    drop(runtime);
    task.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn windows_update_job_boundary_scope_precedes_query_and_check_is_single_use() {
    let (runtime, root, task) = update_state_fixture().await;
    let (other, other_root, other_task) = update_state_fixture().await;
    let binding = update_state_binding(Uuid::new_v4());
    let state = runtime
        .windows_update_state_for_operation(binding.operation_id, "ws_fixture", "srv_fixture")
        .unwrap();
    let mut child = update_state::ControllerUpdateChild::spawn_fixture(
        &state,
        &runtime,
        &binding,
        &mut update_custody_command(),
    )
    .unwrap();
    assert!(child
        .check_fixture_job_boundary(&other, &binding, |_| panic!(
            "foreign runtime queried Child"
        ))
        .is_err());
    let mut foreign = binding.clone();
    foreign.operation_id = Uuid::new_v4();
    assert!(child
        .check_fixture_job_boundary(&runtime, &foreign, |_| panic!(
            "foreign operation queried Child"
        ))
        .is_err());
    child
        .check_fixture_job_boundary(&runtime, &binding, |_| Ok(()))
        .unwrap();
    assert!(child
        .check_fixture_job_boundary(&runtime, &binding, |_| panic!("repeated job query"))
        .is_err());
    assert!(child.poll(&runtime, &binding).unwrap().is_none());
    child.stop(&runtime, &binding).unwrap();
    drop(child);
    drop(state);
    drop(runtime);
    drop(other);
    task.abort();
    other_task.abort();
    std::fs::remove_dir_all(root).unwrap();
    std::fs::remove_dir_all(other_root).unwrap();
}

#[tokio::test]
async fn windows_update_job_boundary_query_error_consumes_check_but_not_teardown() {
    let (runtime, root, task) = update_state_fixture().await;
    let binding = update_state_binding(Uuid::new_v4());
    let state = runtime
        .windows_update_state_for_operation(binding.operation_id, "ws_fixture", "srv_fixture")
        .unwrap();
    let mut child = update_state::ControllerUpdateChild::spawn_fixture(
        &state,
        &runtime,
        &binding,
        &mut update_custody_command(),
    )
    .unwrap();
    let error = child
        .check_fixture_job_boundary(&runtime, &binding, |_| {
            anyhow::bail!("fixed fixture query refusal")
        })
        .unwrap_err();
    assert_eq!(error.to_string(), "fixed fixture query refusal");
    assert!(child
        .check_fixture_job_boundary(&runtime, &binding, |_| panic!("unknown query retried"))
        .is_err());
    child.stop(&runtime, &binding).unwrap();
    assert!(child
        .check_fixture_job_boundary(&runtime, &binding, |_| panic!("stopped Child queried"))
        .is_err());
    drop(child);
    drop(state);
    drop(runtime);
    task.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(windows)]
#[tokio::test]
async fn windows_update_job_boundary_native_original_child_in_job_refuses_without_escape() {
    let (runtime, root, task) = update_state_fixture().await;
    let binding = update_state_binding(Uuid::new_v4());
    let state = runtime
        .windows_update_state_for_operation(binding.operation_id, "ws_fixture", "srv_fixture")
        .unwrap();
    let mut child = update_state::ControllerUpdateChild::spawn_fixture(
        &state,
        &runtime,
        &binding,
        &mut update_custody_command(),
    )
    .unwrap();
    let job = child.assign_fixture_job();
    let error = child
        .check_worker_job_boundary(&runtime, &binding)
        .unwrap_err();
    assert_eq!(error.to_string(), "windows_update_child_job_unconfirmed");
    assert!(child.check_worker_job_boundary(&runtime, &binding).is_err());
    // Refusing a query neither escapes the Job nor signals its owned Child.
    assert!(child.poll(&runtime, &binding).unwrap().is_none());
    child.stop(&runtime, &binding).unwrap();
    drop(job);
    drop(child);
    drop(state);
    drop(runtime);
    task.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(windows)]
#[tokio::test]
async fn windows_update_job_boundary_native_exited_259_child_cannot_pass_live_check() {
    let (runtime, root, task) = update_state_fixture().await;
    let binding = update_state_binding(Uuid::new_v4());
    let state = runtime
        .windows_update_state_for_operation(binding.operation_id, "ws_fixture", "srv_fixture")
        .unwrap();
    let mut child = update_state::ControllerUpdateChild::spawn_fixture(
        &state,
        &runtime,
        &binding,
        &mut update_custody_command(),
    )
    .unwrap();
    child.finish_fixture_input();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.poll(&runtime, &binding).unwrap() {
            break status;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "own helper exit deadline"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    };
    assert_eq!(status.code(), Some(259));
    assert!(child.check_worker_job_boundary(&runtime, &binding).is_err());
    assert_eq!(child.stop(&runtime, &binding).unwrap(), status);
    drop(child);
    drop(state);
    drop(runtime);
    task.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn windows_update_child_scope_refuses_before_spawn_and_before_owned_stop() {
    let (runtime, root, task) = update_state_fixture().await;
    let (other, other_root, other_task) = update_state_fixture().await;
    let binding = update_state_binding(Uuid::new_v4());
    let state = runtime
        .windows_update_state_for_operation(binding.operation_id, "ws_fixture", "srv_fixture")
        .unwrap();
    let mut nonexistent = std::process::Command::new(root.join("must-not-spawn"));
    let refused = update_state::ControllerUpdateChild::spawn_fixture(
        &state,
        &other,
        &binding,
        &mut nonexistent,
    )
    .err()
    .unwrap();
    assert_eq!(refused.to_string(), "windows_update_child_runtime_refused");
    let mut child = update_state::ControllerUpdateChild::spawn_fixture(
        &state,
        &runtime,
        &binding,
        &mut update_custody_command(),
    )
    .unwrap();
    assert!(child.poll(&runtime, &binding).unwrap().is_none());
    assert!(child.stop(&other, &binding).is_err());
    for field in 0..4 {
        let mut foreign = binding.clone();
        match field {
            0 => foreign.operation_id = Uuid::new_v4(),
            1 => foreign.workspace_id = "ws_other".into(),
            2 => foreign.server_id = "srv_other".into(),
            _ => foreign.controller_sha256 = "f".repeat(64),
        }
        assert!(child.poll(&runtime, &foreign).is_err());
        assert!(child.stop(&runtime, &foreign).is_err());
    }
    // Refusals did not touch this Child. Only the exact runtime/binding can stop it.
    assert!(child.poll(&runtime, &binding).unwrap().is_none());
    child.stop(&runtime, &binding).unwrap();
    assert!(child.stop(&runtime, &binding).is_err());
    drop(child);
    drop(state);
    drop(runtime);
    drop(other);
    task.abort();
    other_task.abort();
    std::fs::remove_dir_all(root).unwrap();
    std::fs::remove_dir_all(other_root).unwrap();
}

#[tokio::test]
async fn windows_update_child_retains_created_handle_through_exit_and_refuses_restop() {
    let (runtime, root, task) = update_state_fixture().await;
    let binding = update_state_binding(Uuid::new_v4());
    let state = runtime
        .windows_update_state_for_operation(binding.operation_id, "ws_fixture", "srv_fixture")
        .unwrap();
    let mut child = update_state::ControllerUpdateChild::spawn_fixture(
        &state,
        &runtime,
        &binding,
        &mut update_custody_command(),
    )
    .unwrap();
    child.finish_fixture_input();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.poll(&runtime, &binding).unwrap() {
            break status;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "owned helper exit deadline"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    };
    assert_eq!(status.code(), Some(if cfg!(windows) { 259 } else { 3 }));
    assert_eq!(child.stop(&runtime, &binding).unwrap(), status);
    assert!(child.stop(&runtime, &binding).is_err());
    drop(child);
    drop(state);
    drop(runtime);
    task.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(windows)]
#[tokio::test]
async fn windows_update_child_native_reservation_keeps_pins_and_claim_while_child_owned() {
    use crate::windows_update_admission::{
        tests::{Fixture, NEW, OLD},
        AdmittedInputs,
    };
    use treer_platform::{windows_update_inputs::PinnedPair, windows_update_store::UpdateStore};
    let (runtime, root, task) = update_state_fixture().await;
    let directory = |name: &str| {
        let path = root.join(name);
        std::fs::create_dir(&path).unwrap();
        treer_platform::secure_directory(&path).unwrap();
        path
    };
    let installed = directory("installed");
    let target = directory("target");
    let job = directory("job");
    for (path, bytes) in [(&installed, OLD), (&target, NEW)] {
        for name in ["treer-agent-server.exe", "treer.exe"] {
            std::fs::write(path.join(name), bytes).unwrap();
            treer_platform::secure_file(&path.join(name)).unwrap();
        }
    }
    let fixture = Fixture::new(|_| {});
    let state = runtime
        .windows_update_state_for_operation(Uuid::new_v4(), "ws_fixture", "srv_fixture")
        .unwrap();
    let admitted = AdmittedInputs::prepare_for_controller(
        PinnedPair::open(&installed)
            .unwrap()
            .lock_installation()
            .unwrap(),
        PinnedPair::open(&target).unwrap(),
        &fixture.documents(),
        &fixture.key.verifying_key(),
        state,
    )
    .unwrap();
    let binding = admitted.binding().clone();
    let reserved = admitted.reserve(UpdateStore::open(&job).unwrap()).unwrap();
    // Execute only the SAME test executable, never the inert admitted image bytes.
    let mut child = reserved
        .spawn_owned_child(&runtime, &mut update_custody_command())
        .unwrap();
    assert!(child.poll(&runtime, &binding).unwrap().is_none());
    assert!(std::fs::OpenOptions::new()
        .write(true)
        .open(installed.join("treer-agent-server.exe"))
        .is_err());
    assert!(std::fs::rename(root.join("configured-state"), root.join("moved-state")).is_err());
    assert!(PinnedPair::open(&installed)
        .unwrap()
        .lock_installation()
        .is_err());
    child.stop(&runtime, &binding).unwrap();
    // Pins outlive process termination until this custody object is dropped.
    assert!(std::fs::rename(root.join("configured-state"), root.join("moved-state")).is_err());
    drop(child);
    assert!(installed.join(".treer-controller-update.claim").exists());
    assert_eq!(
        std::fs::read(installed.join("treer-agent-server.exe")).unwrap(),
        OLD
    );
    assert_eq!(
        std::fs::read(target.join("treer-agent-server.exe")).unwrap(),
        NEW
    );
    drop(runtime);
    task.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(windows)]
fn update_launch_reservation(
    runtime: &ControllerRuntime,
    root: &std::path::Path,
    target_bytes: &[u8],
) -> crate::windows_update_admission::ReservedInputs {
    use crate::windows_update_admission::{
        tests::{Fixture, OLD},
        AdmittedInputs,
    };
    use treer_platform::{windows_update_inputs::PinnedPair, windows_update_store::UpdateStore};
    let directory = |name: &str| {
        let path = root.join(name);
        std::fs::create_dir(&path).unwrap();
        treer_platform::secure_directory(&path).unwrap();
        path
    };
    let installed = directory("installed");
    let target = directory("target");
    let job = directory("job");
    for (path, bytes) in [(&installed, OLD), (&target, target_bytes)] {
        for name in ["treer-agent-server.exe", "treer.exe"] {
            std::fs::write(path.join(name), bytes).unwrap();
            treer_platform::secure_file(&path.join(name)).unwrap();
        }
    }
    let fixture = Fixture::with_target(target_bytes, |_| {});
    let state = runtime
        .windows_update_state_for_operation(Uuid::new_v4(), "ws_fixture", "srv_fixture")
        .unwrap();
    AdmittedInputs::prepare_for_controller(
        PinnedPair::open(&installed)
            .unwrap()
            .lock_installation()
            .unwrap(),
        PinnedPair::open(&target).unwrap(),
        &fixture.documents(),
        &fixture.key.verifying_key(),
        state,
    )
    .unwrap()
    .reserve(UpdateStore::open(&job).unwrap())
    .unwrap()
}

#[cfg(windows)]
#[tokio::test]
async fn windows_update_launch_native_signed_pe_reaches_owned_child_but_is_not_ready() {
    let (runtime, root, task) = update_state_fixture().await;
    // Real PE bytes, signed only by the closed fixture authority. This is the
    // Rust test executable, NOT an installed worker. Its test harness rejects
    // the fixed private worker argument, so no normal Controller startup occurs.
    let bytes = std::fs::read(std::env::current_exe().unwrap()).unwrap();
    assert!(bytes.len() <= 128 * 1024 * 1024);
    let reserved = update_launch_reservation(&runtime, &root, &bytes);
    let binding = reserved.binding().clone();
    let mut child = reserved.spawn_owned_worker(&runtime).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.poll(&runtime, &binding).unwrap() {
            break status;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "private worker refusal deadline"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    };
    assert!(
        !status.success(),
        "a test harness is not a ready production worker"
    );
    assert_eq!(child.stop(&runtime, &binding).unwrap(), status);
    assert!(std::fs::rename(root.join("target"), root.join("moved-target")).is_err());
    assert!(std::fs::OpenOptions::new()
        .write(true)
        .open(root.join("target/treer-agent-server.exe"))
        .is_err());
    drop(child);
    assert!(root
        .join("installed/.treer-controller-update.claim")
        .exists());
    drop(runtime);
    task.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(windows)]
#[tokio::test]
async fn windows_update_launch_native_invalid_pe_consumes_attempt_and_keeps_claim() {
    let (runtime, root, task) = update_state_fixture().await;
    let mut reserved =
        update_launch_reservation(&runtime, &root, crate::windows_update_admission::tests::NEW);
    assert_eq!(
        reserved.spawn_worker_process().err().unwrap().to_string(),
        "windows_update_worker_spawn_unknown"
    );
    assert_eq!(
        reserved.spawn_worker_process().err().unwrap().to_string(),
        "windows_update_worker_already_attempted"
    );
    let backup = root.join("backup");
    std::fs::create_dir(&backup).unwrap();
    treer_platform::secure_directory(&backup).unwrap();
    let backed = reserved
        .backup(treer_platform::windows_update_backup::BackupDirectory::open(&backup).unwrap())
        .unwrap()
        .backup_configured_startup()
        .unwrap();
    assert_eq!(
        backed.publish_pair().err().unwrap().to_string(),
        "windows_update_worker_already_attempted"
    );
    assert!(root
        .join("installed/.treer-controller-update.claim")
        .exists());
    assert_eq!(
        std::fs::read(root.join("installed/treer-agent-server.exe")).unwrap(),
        crate::windows_update_admission::tests::OLD
    );
    drop(runtime);
    task.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(windows)]
#[tokio::test]
async fn windows_update_state_native_admission_uses_pinned_runtime_path_not_private_decoy() {
    use crate::windows_update_admission::{
        tests::{Fixture, NEW, OLD},
        AdmittedInputs,
    };
    use treer_platform::{
        windows_update_backup::BackupDirectory, windows_update_inputs::PinnedPair,
        windows_update_store::UpdateStore,
    };
    let (runtime, root, task) = update_state_fixture().await;
    let directory = |name: &str| {
        let path = root.join(name);
        std::fs::create_dir(&path).unwrap();
        treer_platform::secure_directory(&path).unwrap();
        path
    };
    let installed = directory("installed");
    let target = directory("target");
    let backup = directory("backup");
    let job = directory("job");
    let decoy = directory("decoy");
    StartupStore::load(decoy.join("srv_fixture-agent-startup.json"))
        .unwrap()
        .deny_absent("ag_decoy")
        .unwrap();
    for (path, bytes) in [(&installed, OLD), (&target, NEW)] {
        for name in ["treer-agent-server.exe", "treer.exe"] {
            std::fs::write(path.join(name), bytes).unwrap();
            treer_platform::secure_file(&path.join(name)).unwrap();
        }
    }
    let configured = root.join("configured-state");
    let state = runtime
        .windows_update_state_for_operation(Uuid::new_v4(), "ws_fixture", "srv_fixture")
        .unwrap();
    // Physical parent stays pinned against rename from authority capture
    // through signed admission, reservation, backup and file publication.
    assert!(std::fs::rename(&configured, root.join("moved-state")).is_err());
    let fixture = Fixture::new(|_| {});
    let admitted = AdmittedInputs::prepare_for_controller(
        PinnedPair::open(&installed)
            .unwrap()
            .lock_installation()
            .unwrap(),
        PinnedPair::open(&target).unwrap(),
        &fixture.documents(),
        &fixture.key.verifying_key(),
        state,
    )
    .unwrap();
    let binding = admitted.binding().clone();
    let backed = admitted
        .reserve(UpdateStore::open(&job).unwrap())
        .unwrap()
        .backup(BackupDirectory::open(&backup).unwrap())
        .unwrap()
        .backup_configured_startup()
        .unwrap();
    backed.inspect_startup().unwrap();
    let evidence: serde_json::Value =
        serde_json::from_slice(&std::fs::read(backup.join("startup.evidence")).unwrap()).unwrap();
    assert!(evidence["controller_state"]["controller_instance_id"].is_string());
    assert_eq!(
        evidence["binding"]["operation_id"],
        binding.operation_id.to_string()
    );
    let published = backed.publish_pair().unwrap();
    assert!(std::fs::rename(&configured, root.join("moved-state")).is_err());
    drop(published);
    let raw = std::fs::read(backup.join("startup.backup")).unwrap();
    assert_eq!(
        raw,
        std::fs::read(configured.join("srv_fixture-agent-startup.json")).unwrap()
    );
    assert_ne!(
        raw,
        std::fs::read(decoy.join("srv_fixture-agent-startup.json")).unwrap()
    );
    assert!(installed.join(".treer-controller-update.claim").exists());
    drop(runtime);
    task.abort();
    std::fs::remove_dir_all(root).unwrap();
}

async fn agent_fixture_configured(
    configure: impl FnOnce(&mut ControllerConfig),
) -> (ControllerRuntime, PathBuf, tokio::task::JoinHandle<()>) {
    use tokio::io::AsyncWriteExt;
    use treer_host_protocol::{
        decode_message, encode_message, HostBuildInfo, HostMessage, HostRequest,
    };
    let temp = std::env::temp_dir().join(format!("trr-agent-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&temp).unwrap();
    #[cfg(unix)]
    let socket = temp.join("host.sock");
    let root = std::fs::canonicalize(&temp).unwrap();
    #[cfg(windows)]
    let socket = treer_platform::endpoint_name(
        &Uuid::new_v4().to_string(),
        &treer_platform::current_user_sid().unwrap(),
    );
    let mut config = ControllerConfig {
        protected_paths: vec![],
        workspace_id: "workspace".into(),
        server_id: "server".into(),
        agent_server_url: "http://127.0.0.1:1".into(),
        proxy_public_url: "https://proxy.example/".into(),
        treer_binary: None,
        interface_cache_path: root.join("interfaces.json"),
        startup_store_path: root.join("startup.json"),
        private_state_root: root.join("agent-private"),
        provider_cli_root: root.join("provider-cli"),
        root: root.clone(),
    };
    configure(&mut config);
    let mut listener = treer_platform::LocalListener::bind(&socket).unwrap();
    let now = chrono::DateTime::from_timestamp_millis(Utc::now().timestamp_millis()).unwrap();
    let metadata = AgentMetadata {
        agent_id: "command".into(),
        run_generation: 1,
        workspace_id: config.workspace_id.clone(),
        server_id: config.server_id.clone(),
        kind: "command".into(),
        name: "command".into(),
        cwd: ".".into(),
        workload_credential: new_workload_credential(),
        http_proxy_port: None,
    };
    let initial = HostResponse::Synced {
        host_epoch: "agent-host".into(),
        host_build: HostBuildInfo {
            version: "test".into(),
            git_commit: "test".into(),
        },
        processes: vec![HostProcessInfo {
            process_id: "command".into(),
            pid: Some(1234),
            cwd: ".".into(),
            running: true,
            started_at: now,
            last_output_at: now,
            exited_at: None,
            exit_code: None,
            stream_epoch: "epoch-command".into(),
            first_revision: 1,
            next_revision: 1,
            bracketed_paste: false,
            metadata: serde_json::to_string(&metadata).unwrap(),
        }],
        replay: vec![HostOutputReplay {
            process_id: "command".into(),
            stream_epoch: "epoch-command".into(),
            first_available_revision: 1,
            next_revision: 1,
            gap: false,
            chunks: vec![],
        }],
    };
    let inventory = initial.clone();
    let task = tokio::spawn(async move {
        let mut stream = listener.accept().await.unwrap();
        while let Ok(length) = stream.read_u32().await {
            let mut bytes = vec![0; length as usize];
            stream.read_exact(&mut bytes).await.unwrap();
            let request: HostRequest = decode_message(&bytes).unwrap();
            let encoded = encode_message(&HostMessage::Response {
                request_id: request.request_id,
                response: Some(inventory.clone()),
                error: None,
            })
            .unwrap();
            stream.write_u32(encoded.len() as u32).await.unwrap();
            stream.write_all(&encoded).await.unwrap();
        }
    });
    let (host, events) = HostClient::connect(&socket).await.unwrap();
    let (runtime, _) = ControllerRuntime::from_sync(host, initial, events, config).unwrap();
    (runtime, root, task)
}

#[tokio::test]
async fn eleventh_interface_registration_is_refused_before_any_manifest_request() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let (runtime, root, host_task) = agent_fixture().await;
    // The Agent's "interface port" counts connections and hangs up at once,
    // so every manifest fetch that happens is observable and fails.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepted = Arc::new(AtomicUsize::new(0));
    let counter = accepted.clone();
    let accept_task = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            counter.fetch_add(1, Ordering::SeqCst);
            drop(stream);
        }
    });
    // An existing registration that every refused attempt must leave intact.
    let existing = AgentInterfaceDescriptor {
        origin_url: None,
        protocol: AGENT_INTERFACE_PROTOCOL_V1.into(),
        instance_id: "existing".into(),
        port: 1,
        capabilities: vec![],
        ui_path: None,
        registered_at: Utc::now(),
    };
    runtime
        .get("command")
        .unwrap()
        .lock()
        .unwrap()
        .info
        .interface = Some(existing.clone());
    let request = || RegisterAgentInterfaceRequest {
        protocol: AGENT_INTERFACE_PROTOCOL_V1.into(),
        instance_id: "flood".into(),
        port,
        capabilities: vec![],
        ui_path: None,
    };
    for attempt in 1..=INTERFACE_REGISTRATIONS_PER_WINDOW {
        let error = runtime
            .register_interface("command", request())
            .await
            .unwrap_err();
        assert_ne!(
            error.code, "agent_interface_rate_limited",
            "attempt {attempt} is within the window"
        );
        assert_eq!(
            accepted.load(Ordering::SeqCst),
            attempt as usize,
            "attempt {attempt} reached the Agent port"
        );
    }
    // The eleventh is refused with the fixed code and never opens a
    // connection: the port count stays where the tenth left it.
    let refused = runtime
        .register_interface("command", request())
        .await
        .unwrap_err();
    assert_eq!(refused.code, "agent_interface_rate_limited");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        INTERFACE_REGISTRATIONS_PER_WINDOW as usize
    );
    // Unknown Agents are answered before the limiter counts anything.
    let unknown = runtime
        .register_interface("missing", request())
        .await
        .unwrap_err();
    assert_ne!(unknown.code, "agent_interface_rate_limited");
    assert_eq!(runtime.interface("command").unwrap(), Some(existing));
    accept_task.abort();
    host_task.abort();
    drop(runtime);
    std::fs::remove_dir_all(root).unwrap();
}
