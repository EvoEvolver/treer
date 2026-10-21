use super::*;
use std::time::Instant;

#[test]
#[ignore = "isolated PTY child, invoked by the backpressure regression"]
fn blocked_child() {
    #[cfg(unix)]
    assert!(std::process::Command::new("/bin/stty")
        .args(["raw", "-echo"])
        .status()
        .unwrap()
        .success());
    println!("PTY_BACKPRESSURE_READY");
    std::thread::sleep(Duration::from_secs(45));
}

fn request(id: &str, command: HostCommand) -> HostRequest {
    HostRequest {
        protocol: HOST_PROTOCOL_VERSION,
        request_id: id.into(),
        operation_id: command.is_mutating().then(|| id.into()),
        command,
    }
}

async fn bounded(state: &HostState, request: &HostRequest) -> Result<HostResponse, HostError> {
    tokio::time::timeout(Duration::from_secs(3), execute_request(state, request))
        .await
        .expect("unrelated control/lifecycle operation must remain responsive")
}

async fn fixture() -> (Arc<HostState>, PathBuf, mpsc::Receiver<()>) {
    let root = std::env::temp_dir().join(format!("treer224-{}", Uuid::new_v4().simple()));
    std::fs::create_dir(&root).unwrap();
    let (restart_controller, restarts) = mpsc::channel(1);
    let state = Arc::new(HostState {
        runtime: HostRuntime::new(&root).unwrap(),
        results: Arc::new(Mutex::new(ResultCache::new())),
        operations: Arc::new(Semaphore::new(16)),
        controls: Arc::new(Semaphore::new(8)),
        restart_controller,
    });
    (state, root, restarts)
}

async fn spawn_blocked(state: &HostState, id: &str) {
    bounded(
        state,
        &request(
            &format!("spawn-{id}"),
            HostCommand::Spawn {
                request: treer_host_protocol::HostSpawnRequest {
                    process_id: id.into(),
                    command: std::env::current_exe().unwrap().display().to_string(),
                    args: vec![
                        "--exact".into(),
                        "isolation_tests::blocked_child".into(),
                        "--ignored".into(),
                        "--nocapture".into(),
                    ],
                    cwd: ".".into(),
                    env: Default::default(),
                    cols: 80,
                    rows: 24,
                    metadata: "isolated224".into(),
                },
            },
        ),
    )
    .await
    .unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        let replay = state.runtime.read(id, None).unwrap();
        let bytes: Vec<_> = replay
            .chunks
            .iter()
            .flat_map(|chunk| chunk.data.iter().copied())
            .collect();
        if String::from_utf8_lossy(&bytes).contains("PTY_BACKPRESSURE_READY") {
            break;
        }
        assert!(Instant::now() < until, "PTY fixture did not become ready");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn write_request() -> HostRequest {
    request(
        "blocked-write",
        HostCommand::Write {
            process_id: "blocked".into(),
            writes: vec![treer_host_protocol::HostWrite {
                data: vec![b'x'; 2 * 1024 * 1024],
                delay_ms: 0,
            }],
        },
    )
}

async fn admitted(state: &HostState, operation: &str) {
    let until = Instant::now() + Duration::from_secs(3);
    while !state.results.lock().await.bindings.contains_key(operation) {
        assert!(Instant::now() < until);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
}

#[tokio::test]
async fn real_pty_backpressure_preserves_other_workload_control_and_unknown_no_replay() {
    let (state, root, mut restarts) = fixture().await;
    spawn_blocked(&state, "blocked").await;
    let write = {
        let state = Arc::clone(&state);
        tokio::spawn(async move { execute_request(&state, &write_request()).await })
    };
    admitted(&state, "blocked-write").await;
    assert!(
        !write.is_finished(),
        "fixture must produce actual backpressure"
    );
    assert!(matches!(
        bounded(
            &state,
            &request(
                "sync",
                HostCommand::Sync {
                    cursors: Default::default()
                }
            )
        )
        .await
        .unwrap(),
        HostResponse::Synced { .. }
    ));
    bounded(&state, &request("restart", HostCommand::RestartController))
        .await
        .unwrap();
    assert_eq!(restarts.try_recv(), Ok(()));
    // A second workload can spawn, resize, read and reach an observed exit.
    spawn_blocked(&state, "other").await;
    bounded(
        &state,
        &request(
            "resize-other",
            HostCommand::Resize {
                process_id: "other".into(),
                cols: 90,
                rows: 30,
            },
        ),
    )
    .await
    .unwrap();
    bounded(
        &state,
        &request(
            "read-other",
            HostCommand::Read {
                process_id: "other".into(),
                cursor: None,
            },
        ),
    )
    .await
    .unwrap();
    let HostResponse::Process { process } = bounded(
        &state,
        &request(
            "stop-other",
            HostCommand::Stop {
                process_id: "other".into(),
            },
        ),
    )
    .await
    .unwrap() else {
        panic!()
    };
    assert!(!process.running);
    assert!(
        process.exit_code.is_some(),
        "stop success requires actual reaping"
    );
    let pending = execute_request(&state, &write_request()).await.unwrap_err();
    assert_eq!(pending.code, "operation_unknown");
    let result = tokio::time::timeout(Duration::from_secs(7), write)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(result.code, "input_unknown");
    // Timeout is not cancellation: the nonreading child is still alive. Public
    // management and a fresh workload must remain usable in this steady state.
    assert!(state
        .runtime
        .list()
        .iter()
        .any(|p| p.process_id == "blocked" && p.running));
    bounded(
        &state,
        &request("restart-still-blocked", HostCommand::RestartController),
    )
    .await
    .unwrap();
    assert_eq!(restarts.try_recv(), Ok(()));
    spawn_blocked(&state, "after-timeout").await;
    bounded(
        &state,
        &request(
            "stop-after-timeout",
            HostCommand::Stop {
                process_id: "after-timeout".into(),
            },
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        execute_request(&state, &write_request()).await.unwrap_err(),
        result
    );
    for index in 0..RESULT_CACHE_LIMIT + 1 {
        let mut another = request(
            "refuse-new-input",
            HostCommand::Write {
                process_id: "blocked".into(),
                writes: vec![treer_host_protocol::HostWrite {
                    data: vec![b'x'],
                    delay_ms: 0,
                }],
            },
        );
        another.operation_id = Some(format!("refused-input-{index}"));
        assert_eq!(
            execute_request(&state, &another).await.unwrap_err().code,
            "input_refused"
        );
    }
    assert_eq!(
        execute_request(&state, &write_request()).await.unwrap_err(),
        result
    );
    let HostResponse::Process { process } = bounded(
        &state,
        &request(
            "stop-blocked",
            HostCommand::Stop {
                process_id: "blocked".into(),
            },
        ),
    )
    .await
    .unwrap() else {
        panic!()
    };
    assert!(!process.running);
    assert_eq!(state.operations.available_permits(), 16);
    assert_eq!(state.controls.available_permits(), 8);
    drop(state);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn same_connection_sync_stop_and_disconnect_do_not_cancel_or_replay_admitted_write() {
    let (state, root, _) = fixture().await;
    spawn_blocked(&state, "blocked").await;
    spawn_blocked(&state, "other").await;
    let (mut client, server) = tokio::io::duplex(2 * 1024 * 1024);
    let connection = tokio::spawn(handle_connection(Box::new(server), Arc::clone(&state)));
    write_frame(&mut client, &write_request()).await.unwrap();
    admitted(&state, "blocked-write").await;
    write_frame(
        &mut client,
        &request(
            "sync-same",
            HostCommand::Sync {
                cursors: Default::default(),
            },
        ),
    )
    .await
    .unwrap();
    let response = tokio::time::timeout(
        Duration::from_secs(2),
        read_frame::<_, HostMessage>(&mut client),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert!(
        matches!(response, HostMessage::Response { request_id, response: Some(HostResponse::Synced { .. }), .. } if request_id == "sync-same")
    );
    write_frame(
        &mut client,
        &request(
            "stop-same",
            HostCommand::Stop {
                process_id: "other".into(),
            },
        ),
    )
    .await
    .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let message = read_frame::<_, HostMessage>(&mut client).await.unwrap().unwrap();
            if matches!(&message, HostMessage::Response { request_id, .. } if request_id == "stop-same") {
                break message;
            }
        }
    }).await.unwrap();
    assert!(
        matches!(response, HostMessage::Response { request_id, response: Some(HostResponse::Process { process }), .. }
        if request_id == "stop-same" && !process.running && process.exit_code.is_some())
    );
    drop(client); // no cancellation claim: admitted write remains in the ledger
    tokio::time::timeout(Duration::from_secs(2), connection)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        execute_request(&state, &write_request())
            .await
            .unwrap_err()
            .code,
        "operation_unknown"
    );
    bounded(
        &state,
        &request(
            "stop-after-disconnect",
            HostCommand::Stop {
                process_id: "blocked".into(),
            },
        ),
    )
    .await
    .unwrap();
    let until = Instant::now() + Duration::from_secs(3);
    while !state
        .results
        .lock()
        .await
        .values
        .contains_key("blocked-write")
    {
        assert!(Instant::now() < until);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        execute_request(&state, &write_request())
            .await
            .unwrap_err()
            .code,
        "input_unknown"
    );
    drop(state);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn exhausted_mutation_lane_has_no_queue_and_preserves_controls() {
    let (state, root, mut restarts) = fixture().await;
    let permits = Arc::clone(&state.operations)
        .try_acquire_many_owned(16)
        .unwrap();
    assert_eq!(
        execute_request(&state, &write_request())
            .await
            .unwrap_err()
            .code,
        "host_busy"
    );
    assert!(!state
        .results
        .lock()
        .await
        .bindings
        .contains_key("blocked-write"));
    bounded(&state, &request("restart", HostCommand::RestartController))
        .await
        .unwrap();
    assert_eq!(restarts.try_recv(), Ok(()));
    bounded(
        &state,
        &request(
            "sync",
            HostCommand::Sync {
                cursors: Default::default(),
            },
        ),
    )
    .await
    .unwrap();
    drop(permits);
    drop(state);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn unknown_tombstone_survives_completed_result_eviction() {
    let mut cache = ResultCache::new();
    cache.bindings.insert("unknown".into(), [7; 32]);
    cache.insert("unknown".into(), Err(operation_unknown()));
    // Match admission's bounded eviction while completing many newer ops.
    for index in 0..RESULT_CACHE_LIMIT * 2 {
        if cache.bindings.len() >= RESULT_CACHE_LIMIT {
            let oldest = cache.order.pop_front().unwrap();
            cache.values.remove(&oldest);
            cache.bindings.remove(&oldest);
        }
        let id = format!("completed-{index}");
        cache.bindings.insert(id.clone(), [0; 32]);
        cache.insert(id, Ok(HostResponse::Ack));
    }
    assert_eq!(cache.bindings.get("unknown"), Some(&[7; 32]));
    assert_eq!(cache.values.get("unknown"), Some(&Err(operation_unknown())));
    assert_eq!(cache.bindings.len(), RESULT_CACHE_LIMIT);
}
