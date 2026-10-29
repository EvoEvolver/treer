//! Real-PTY workloads through the Controller's output path and idle monitor:
//! the emitted status sequence must not flap.

use super::*;
use std::io::Read;
use tokio::io::AsyncWriteExt;
use treer_host_protocol::{
    decode_message, encode_message, HostBuildInfo, HostMessage, HostRequest,
};

const SCENARIOS: [(&str, &str); 3] = [
    // A clock repainting in place at an idle prompt.
    (
        "clock",
        r#"printf '$ '; while :; do printf '\r%s' "$(date +%T)"; sleep 1; done"#,
    ),
    // A top-like full-screen redraw with changing numbers every second.
    (
        "top",
        r#"i=0; while :; do i=$((i+1)); printf '\033[H\033[2JPID  CPU  TIME\n'; for p in 1 2 3 4 5; do printf '%s  %s  %s\n' $p $(( (i*p) % 97 )) $i; done; sleep 1; done"#,
    ),
    // A build that streams for a few seconds and then returns to the prompt.
    (
        "build",
        r#"for i in $(seq 1 40); do echo "compiling unit $i"; sleep 0.1; done; printf 'done\n$ '; sleep 60"#,
    ),
];

fn process(id: &str, now: chrono::DateTime<Utc>) -> (HostProcessInfo, HostOutputReplay) {
    let metadata = AgentMetadata {
        agent_id: id.into(),
        run_generation: 1,
        workspace_id: "workspace".into(),
        server_id: "server".into(),
        kind: "terminal".into(),
        name: id.into(),
        cwd: ".".into(),
        workload_credential: new_workload_credential(),
        http_proxy_port: None,
    };
    let info = HostProcessInfo {
        process_id: id.into(),
        pid: Some(1234),
        cwd: ".".into(),
        running: true,
        started_at: now,
        last_output_at: now,
        exited_at: None,
        exit_code: None,
        stream_epoch: format!("epoch-{id}"),
        first_revision: 1,
        next_revision: 2,
        bracketed_paste: false,
        metadata: serde_json::to_string(&metadata).unwrap(),
    };
    // The restored shell sits at its prompt, so every run starts Idle.
    let replay = HostOutputReplay {
        process_id: id.into(),
        stream_epoch: format!("epoch-{id}"),
        first_available_revision: 1,
        next_revision: 2,
        gap: false,
        chunks: vec![HostOutputChunk {
            process_id: id.into(),
            stream_epoch: format!("epoch-{id}"),
            revision: 1,
            data: b"user@host:~$ ".to_vec(),
            bracketed_paste: false,
            emitted_at: now,
        }],
    };
    (info, replay)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_pty_redraws_and_builds_do_not_flap() {
    use portable_pty::{native_pty_system, CommandBuilder, PtySize};

    let root = std::env::temp_dir().join(format!(
        "trr-a-{}",
        &Uuid::new_v4().simple().to_string()[..12]
    ));
    std::fs::create_dir_all(&root).unwrap();
    let socket = root.join("host.sock");
    let mut listener = treer_platform::LocalListener::bind(&socket).unwrap();
    let now = chrono::DateTime::from_timestamp_millis(Utc::now().timestamp_millis()).unwrap();
    let (processes, replay): (Vec<_>, Vec<_>) =
        SCENARIOS.iter().map(|(id, _)| process(id, now)).unzip();
    let initial = HostResponse::Synced {
        host_epoch: "host".into(),
        host_build: HostBuildInfo {
            version: "test".into(),
            git_commit: "test".into(),
        },
        processes,
        replay,
    };
    let answer = initial.clone();
    let host_task = tokio::spawn(async move {
        let mut stream = listener.accept().await.unwrap();
        while let Ok(length) = stream.read_u32().await {
            let mut bytes = vec![0; length as usize];
            stream.read_exact(&mut bytes).await.unwrap();
            let request: HostRequest = decode_message(&bytes).unwrap();
            let encoded = encode_message(&HostMessage::Response {
                request_id: request.request_id,
                response: Some(answer.clone()),
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
        root: root.clone(),
    };
    let (runtime, _) = ControllerRuntime::from_sync(host, initial, events, config).unwrap();
    for (id, _) in SCENARIOS {
        let status = runtime
            .list()
            .into_iter()
            .find(|a| a.agent_id == id)
            .unwrap()
            .status;
        assert_eq!(status, AgentStatus::Idle, "{id} starts at its prompt");
    }
    let mut statuses = runtime.subscribe();
    let (sender, mut chunks) = tokio::sync::mpsc::unbounded_channel::<(String, Vec<u8>)>();
    let mut children = Vec::new();
    for (id, script) in SCENARIOS {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut command = CommandBuilder::new("/bin/sh");
        command.args(["-c", script]);
        children.push(pair.slave.spawn_command(command).unwrap());
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().unwrap();
        let sender = sender.clone();
        std::thread::spawn(move || {
            let _master = pair.master;
            let mut buffer = [0_u8; 4096];
            while let Ok(read) = reader.read(&mut buffer) {
                if read == 0
                    || sender
                        .send((id.to_string(), buffer[..read].to_vec()))
                        .is_err()
                {
                    break;
                }
            }
        });
    }
    drop(sender);
    let started = Instant::now();
    let mut revisions: HashMap<String, u64> = HashMap::new();
    let mut changes: HashMap<String, Vec<(Duration, AgentStatus)>> = HashMap::new();
    let mut last: HashMap<String, AgentStatus> = SCENARIOS
        .iter()
        .map(|(id, _)| (id.to_string(), AgentStatus::Idle))
        .collect();
    let deadline = tokio::time::sleep(Duration::from_secs(14));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => break,
            Some((id, data)) = chunks.recv() => {
                let revision = revisions.entry(id.clone()).or_insert(1);
                *revision += 1;
                runtime.apply_output(HostOutputChunk {
                    process_id: id.clone(),
                    stream_epoch: format!("epoch-{id}"),
                    revision: *revision,
                    data,
                    bracketed_paste: false,
                    emitted_at: Utc::now(),
                });
            }
            Ok(info) = statuses.recv() => {
                if last.get(&info.agent_id) != Some(&info.status) {
                    last.insert(info.agent_id.clone(), info.status);
                    changes
                        .entry(info.agent_id.clone())
                        .or_default()
                        .push((started.elapsed(), info.status));
                }
            }
        }
    }
    for mut child in children {
        let _ = child.kill();
    }
    host_task.abort();
    let _ = std::fs::remove_dir_all(&root);
    let sequence = |id: &str| -> Vec<AgentStatus> {
        changes
            .get(id)
            .map(|changes| changes.iter().map(|(_, status)| *status).collect())
            .unwrap_or_default()
    };
    assert_eq!(
        sequence("clock"),
        vec![],
        "clock: {:?}",
        changes.get("clock")
    );
    assert_eq!(
        sequence("top"),
        vec![AgentStatus::Working],
        "top: {:?}",
        changes.get("top")
    );
    assert_eq!(
        sequence("build"),
        vec![AgentStatus::Working, AgentStatus::Idle],
        "build: {:?}",
        changes.get("build")
    );
    for list in changes.values() {
        for pair in list.windows(2) {
            assert!(pair[1].0 - pair[0].0 >= MIN_STATUS_DWELL, "flap: {list:?}");
        }
    }
}
