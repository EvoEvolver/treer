//! Isolated real daemon tests. Never resolve an installed service or use its state.
use super::*;
use crate::{
    host_client::HostClient,
    startup_store::{StartupStore, StoredAgentStartup},
};
use axum::{
    extract::ws::WebSocketUpgrade,
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use std::{
    collections::BTreeSet,
    os::unix::fs::PermissionsExt,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
use treer_host_protocol::HostResponse;
use treer_protocol::AgentStartupSpec;

fn fixture_paths(root: &Path) -> ServicePaths {
    let binaries = env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    ServicePaths {
        executable: root.join("controller"),
        host_executable: binaries.join("treer-agent-host"),
        config: root.join("controller.json"),
        host_config: root.join("host.json"),
        state_dir: root.into(),
    }
}

fn pollute(command: &mut Command, root: &Path) {
    for key in [
        "TREER_AGENT_ID",
        "TREER_WORKLOAD_CREDENTIAL",
        "TREER_AGENT_SERVER_URL",
        "TREER_WORKSPACE_ID",
        "TREER_SERVER_ID",
        "TREER_IDENTITY_MODE",
        "TREER_HTTP_PROXY",
        "TREER_BIN",
        "TREER_MACHINE_TOKEN",
        "TREER_OPERATOR_CREDENTIAL",
        "TREER_ENROLLMENT_KEY",
        "AGENT_UI_INSTANCE_ID",
        "AGENT_UI_SESSION_ID",
        "AGENT_UI_HARNESS",
        "CODEX_UI_THREAD_ID",
    ] {
        command.env(key, "polluted-fixture");
    }
    for key in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ] {
        command.env(key, "http://127.0.0.1:1");
    }
    command
        .env("TREER_STATE_DIR", root)
        .env_remove("NO_PROXY")
        .env_remove("no_proxy");
}

// A separate test process supplies polluted caller state without mutating the
// multithreaded test runner's environment.
#[test]
#[ignore = "subprocess helper for real_service_environment_boundary"]
fn polluted_service_launcher() {
    let Some(root) = env::var_os("TREER_ENV_TEST_ROOT").map(PathBuf::from) else {
        return;
    };
    let paths = fixture_paths(&root);
    let config = ServiceConfig::load(&paths.config).unwrap();
    warn_managed_agent_service_stop();
    start_nohup(&paths, &config).unwrap();
}

fn assert_clean_names(names: &BTreeSet<String>, workload: bool) {
    for name in names {
        if workload
            && (name.starts_with("TREER_")
                || name.eq_ignore_ascii_case("http_proxy")
                || name.eq_ignore_ascii_case("https_proxy"))
        {
            continue; // Fresh, explicitly injected workload identity and entry.
        }
        assert!(
            treer_platform::inherited_variable_allowed(name, "http://127.0.0.1:1"),
            "inherited {name}"
        );
    }
    assert!(names.contains("PATH"));
}

fn environment_names(path: &Path) -> BTreeSet<String> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[tokio::test]
#[ignore = "requires freshly built sibling treer-agent-host and treer-agent-server binaries"]
async fn real_service_environment_boundary() {
    for direct in [false, true] {
        let root = PathBuf::from("/tmp").join(format!(
            "trr-env-{}",
            &Uuid::new_v4().simple().to_string()[..12]
        ));
        treer_platform::secure_directory(&root).unwrap();
        let paths = fixture_paths(&root);
        let binaries = env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        assert!(
            paths.host_executable.is_file() && binaries.join("treer-agent-server").is_file(),
            "build sibling binaries first"
        );
        let validation_calls = Arc::new(AtomicUsize::new(0));
        let calls = validation_calls.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/agent/connect", get(|ws: WebSocketUpgrade| async move {
                ws.on_upgrade(|mut socket| async move { while socket.recv().await.is_some() {} })
            }))
            .route("/agent/workspaces/fixture-workspace/machines/srv_env/startup/validate", post(move || {
                let calls = calls.clone();
                async move {
                    // Fail both initial validation and the immediate heartbeat;
                    // recovery must finish later without a WS reconnect.
                    if calls.fetch_add(1, Ordering::SeqCst) < 2 {
                        (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({"error":"fixture transient failure"})))
                    } else {
                        (StatusCode::OK, Json(serde_json::json!({"active_agent_ids":["fixture-agent","fixture-app"], "stopped_agent_ids":[]})))
                    }
                }
            }));
        let proxy = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let config = ServiceConfig {
            proxy: format!("http://{proxy_address}/"),
            workspace: "fixture-workspace".into(),
            server_id: "srv_env".into(),
            machine_token: "fixture-machine".into(),
            operator_credential: "fixture-operator".into(),
            root: root.clone(),
            listen: "127.0.0.1:0".into(),
            host_socket: root.join("host.sock"),
            install_hostname: current_hostname().unwrap(),
            network_mode: None,
            service_manager: ServiceManager::Nohup,
            service_fallback_reason: None,
        };
        save_json(&config, &paths.config).unwrap();
        // Record names only, not any credentials. The wrapper then becomes the
        // real Controller, preserving PID and its complete launch environment.
        fs::write(
            &paths.executable,
            format!(
                "#!/bin/sh\n/usr/bin/env | cut -d= -f1 > \"${{0}}.env\"\nexec '{}' \"$@\"\n",
                binaries.join("treer-agent-server").display()
            ),
        )
        .unwrap();
        fs::set_permissions(&paths.executable, fs::Permissions::from_mode(0o700)).unwrap();
        save_json(
            &HostDaemonConfig {
                socket_path: config.host_socket.clone(),
                controller_path: paths.executable.clone(),
                controller_config_path: paths.config.clone(),
                root: root.clone(),
            },
            &paths.host_config,
        )
        .unwrap();
        let store = StartupStore::load(root.join("srv_env-agent-startup.json")).unwrap();
        for (id, kind) in [("fixture-agent", "command"), ("fixture-app", "app")] {
            store
                .upsert(StoredAgentStartup {
                    app_revision: 0,
                    spec: AgentStartupSpec {
                        agent_id: id.into(),
                        server_id: "srv_env".into(),
                        kind: kind.into(),
                        name: id.into(),
                        cwd: ".".into(),
                        command: "/bin/sh".into(),
                        args: vec![
                            "-c".into(),
                            format!("/usr/bin/env | cut -d= -f1 > {id}.env; exec sleep 120"),
                        ],
                        publish_ports: vec![],
                        enabled: true,
                        generation: 1,
                    },
                    workspace_id: config.workspace.clone(),
                    workload_credential: format!("credential-{id}"),
                    last_host_epoch: "previous-host".into(),
                    initial_writes: vec![],
                    cols: 80,
                    rows: 24,
                })
                .unwrap();
        }
        struct Cleanup {
            root: PathBuf,
            pid: Option<u32>,
            proxy: tokio::task::JoinHandle<()>,
        }
        impl Drop for Cleanup {
            fn drop(&mut self) {
                if let Some(pid) = self.pid {
                    let _ = Command::new("kill")
                        .args(["-TERM", &pid.to_string()])
                        .status();
                    // Host handles SIGTERM by stopping its Controller and PTYs.
                    for _ in 0..100 {
                        if !host_is_running(&self.root.join("host.sock")) {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(20));
                    }
                }
                self.proxy.abort();
                let _ = fs::remove_dir_all(&self.root);
            }
        }
        let mut cleanup = Cleanup {
            root: root.clone(),
            pid: None,
            proxy,
        };
        let mut command = if direct {
            let mut command = Command::new(&paths.host_executable);
            command.args(["run", "--config"]).arg(&paths.host_config);
            command
        } else {
            let mut command = Command::new(env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "service::environment_tests::polluted_service_launcher",
                    "--ignored",
                    "--nocapture",
                ])
                .env("TREER_ENV_TEST_ROOT", &root);
            command
        };
        pollute(&mut command, &root);
        let log = fs::File::create(root.join("daemon.log")).unwrap();
        command.stdout(log.try_clone().unwrap()).stderr(log);
        let mut child = command.spawn().unwrap();
        if direct {
            cleanup.pid = Some(child.id());
        } else {
            assert!(child.wait().unwrap().success(), "fixture launcher failed");
            cleanup.pid = Some(read_nohup_process(&paths, &config).unwrap().unwrap().pid);
        }
        let deadline = Instant::now() + Duration::from_secs(20);
        let (host, _events) = loop {
            if let Ok(connection) = HostClient::connect(&config.host_socket).await {
                break connection;
            }
            assert!(Instant::now() < deadline, "isolated Host did not start");
            tokio::time::sleep(Duration::from_millis(25)).await;
        };
        let processes = loop {
            if let HostResponse::Synced { processes, .. } =
                host.sync(Default::default()).await.unwrap()
            {
                if processes.len() == 2
                    && processes.iter().all(|p| p.running)
                    && root.join("fixture-agent.env").exists()
                    && root.join("fixture-app.env").exists()
                {
                    break processes;
                }
            }
            assert!(
                Instant::now() < deadline,
                "restore failed to retry and complete"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        assert!(validation_calls.load(Ordering::SeqCst) >= 3);
        assert_clean_names(&environment_names(&root.join("controller.env")), false);
        // OS-visible initial environment must also be clean, including when a
        // polluted service manager invokes `host run` directly.
        #[cfg(target_os = "linux")]
        {
            let bytes = fs::read(format!("/proc/{}/environ", cleanup.pid.unwrap())).unwrap();
            let names = bytes
                .split(|b| *b == 0)
                .filter_map(|entry| entry.split(|b| *b == b'=').next())
                .filter(|name| !name.is_empty())
                .map(|name| String::from_utf8_lossy(name).into_owned())
                .collect();
            assert_clean_names(&names, false);
        }
        #[cfg(target_os = "macos")]
        {
            let output = Command::new("ps")
                .args([
                    "eww",
                    "-p",
                    &cleanup.pid.unwrap().to_string(),
                    "-o",
                    "command=",
                ])
                .output()
                .unwrap();
            let names = String::from_utf8(output.stdout)
                .unwrap()
                .split_whitespace()
                .filter_map(|item| item.split_once('=').map(|(key, _)| key.to_owned()))
                .collect();
            assert_clean_names(&names, false);
        }
        for id in ["fixture-agent", "fixture-app"] {
            assert_clean_names(&environment_names(&root.join(format!("{id}.env"))), true);
        }
        let before = fs::metadata(root.join("controller.env"))
            .unwrap()
            .modified()
            .unwrap();
        let validations_before_restart = validation_calls.load(Ordering::SeqCst);
        restart_controller_at(&paths, &config.host_socket).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while validation_calls.load(Ordering::SeqCst) == validations_before_restart
            || fs::metadata(root.join("controller.env"))
                .unwrap()
                .modified()
                .unwrap()
                == before
        {
            assert!(Instant::now() < deadline, "Controller did not hot restart");
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        assert_clean_names(&environment_names(&root.join("controller.env")), false);
        let HostResponse::Synced {
            processes: after, ..
        } = host.sync(Default::default()).await.unwrap()
        else {
            panic!("expected inventory");
        };
        for process in &processes {
            assert_eq!(
                after
                    .iter()
                    .find(|p| p.process_id == process.process_id)
                    .unwrap()
                    .pid,
                process.pid
            );
        }
        let log = fs::read_to_string(if direct {
            root.join("daemon.log")
        } else {
            service_log_path(&paths, &config)
        })
        .unwrap();
        assert!(log.contains("restore will retry"));
        assert!(log.contains("restored deferred Agent startup command"));
        if !direct {
            let caller_log = fs::read_to_string(root.join("daemon.log")).unwrap();
            assert!(caller_log.contains("will terminate the current managed Agent"));
            assert!(caller_log.contains("execution will continue"));
        }
        drop(cleanup);
        child.wait().unwrap();
    }
}
