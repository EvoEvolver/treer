use super::*;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;

#[tokio::test]
async fn explicit_listen_conflict_fails_before_enrollment_and_allows_retry() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let error = resolve_listen("ws_fixture", Some(address))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("port is already in use"));
    assert!(error.to_string().contains("best-effort preflight"));
    drop(listener);
    assert_eq!(
        resolve_listen("ws_fixture", Some(address)).await.unwrap(),
        address
    );
}

#[test]
fn shared_account_warning_checks_only_other_config_names() {
    let directory = env::temp_dir().join(format!("treer-config-names-{}", Uuid::new_v4()));
    assert!(!has_other_controller_config(&directory, "srv_one").unwrap());
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("srv_one-controller.json"), b"not even JSON").unwrap();
    fs::write(directory.join("srv_two-host.json"), b"not a controller").unwrap();
    assert!(!has_other_controller_config(&directory, "srv_one").unwrap());
    let other = directory.join("srv_two-controller.json");
    fs::write(&other, b"must never parse another enrollment").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&other, fs::Permissions::from_mode(0o000)).unwrap();
    }
    assert!(has_other_controller_config(&directory, "srv_one").unwrap());
    fs::remove_dir_all(directory).unwrap();
}

#[cfg(windows)]
#[test]
fn windows_supervision_and_identity_paths() {
    assert_eq!(
        select_service_manager(ServiceMode::Auto).unwrap().manager,
        ServiceManager::Detached
    );
    assert_eq!(
        select_service_manager(ServiceMode::Foreground)
            .unwrap()
            .manager,
        ServiceManager::Foreground
    );
    assert!(select_service_manager(ServiceMode::Systemd).is_err());
    let pipe = host_socket_path("srv_windows").unwrap();
    assert!(pipe.to_string_lossy().starts_with(r"\\.\pipe\treer-"));
    assert_eq!(pipe, host_socket_path("srv_windows").unwrap());
    assert_ne!(pipe, host_socket_path("srv_other").unwrap());
    assert!(config_dir().unwrap().ends_with("Treer/agent-servers"));
}

#[cfg(windows)]
#[test]
#[ignore = "requires freshly built sibling Controller and Host binaries"]
fn windows_real_host_detached_restart_and_stop() {
    let private = env::temp_dir().join(format!("treer-win-smoke-{}", Uuid::new_v4().simple()));
    for child in ["local", "roaming", "temp"] {
        treer_platform::secure_directory(&private.join(child)).unwrap();
    }
    let mut command = Command::new(env::current_exe().unwrap());
    command
        .env_clear()
        .envs(treer_platform::machine_exec_environment())
        .args([
            "--exact",
            "service::tests::windows_isolated_host_fixture",
            "--ignored",
            "--nocapture",
        ])
        .env("LOCALAPPDATA", private.join("local"))
        .env("TREER_TEST_WINDOWS_CONFIG_DIR", private.join("local"))
        .env("APPDATA", private.join("roaming"))
        .env("TEMP", private.join("temp"))
        .env("TMP", private.join("temp"))
        .env("TREER_MACHINE_TOKEN", "synthetic-stale-machine")
        .env("TREER_OPERATOR_CREDENTIAL", "synthetic-stale-operator")
        .env("TREER_ENROLLMENT_KEY", "synthetic-stale-enrollment")
        .env("AGENT_UI_INSTANCE_ID", "synthetic-stale-ui")
        .current_dir(PathBuf::from(env::var_os("SystemRoot").unwrap()).join("System32"));
    let output = command.output().unwrap();
    let _ = fs::remove_dir_all(private);
    assert!(
        output.status.success(),
        "isolated Windows fixture failed: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("windows process matrix completed: exec=4 command=4 app=4 restart=1"),
        "isolated matrix must actually run, not pass an empty filter"
    );
}

#[cfg(windows)]
#[tokio::test]
#[ignore = "child fixture with a private configuration directory; called by the wrapper"]
async fn windows_isolated_host_fixture() {
    use crate::host_client::HostClient;
    use treer_host_protocol::{HostCommand, HostResponse, HostSpawnRequest};
    let isolated = env::var_os("TREER_TEST_WINDOWS_CONFIG_DIR")
        .expect("run this child fixture through windows_real_host_detached_restart_and_stop");
    assert_eq!(env::var_os("LOCALAPPDATA"), Some(isolated));
    let default_root = new_enrollment_root(None).unwrap();
    assert!(default_root.ends_with("Treer/root"));
    assert_ne!(
        default_root,
        fs::canonicalize(env::current_dir().unwrap()).unwrap()
    );
    let junction_target = env::temp_dir().join("default-root-junction-target");
    fs::create_dir(&junction_target).unwrap();
    fs::remove_dir(&default_root).unwrap();
    let junction = Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(&default_root)
        .arg(&junction_target)
        .output()
        .unwrap();
    assert!(
        junction.status.success(),
        "native fixture could not create a junction"
    );
    assert!(
        new_enrollment_root(None).is_err(),
        "default root must reject a junction before setting ACLs"
    );
    fs::remove_dir(&default_root).unwrap();
    fs::remove_dir(&junction_target).unwrap();
    new_enrollment_root(None).unwrap();
    let root = env::temp_dir().join(format!("treer-windows-host-{}", Uuid::new_v4().simple()));
    treer_platform::secure_directory(&root).unwrap();
    // Match run_server's canonical-root precondition. On Windows the temp
    // path can be a plain/short drive path while canonicalize(cwd) uses a
    // verbatim prefix. Comparing that canonical cwd to a raw fixture root
    // falsely rejects even ".". Do not weaken the production root check.
    let raw_root = root;
    let root = fs::canonicalize(&raw_root).unwrap();
    let cwd = fs::canonicalize(root.join(".")).unwrap();
    assert!(cwd.is_dir() && cwd.starts_with(&root));
    eprintln!("windows-host-root raw-equals-canonical={} raw-contains-canonical-cwd={} canonical-contains-cwd={}", raw_root == root, cwd.starts_with(&raw_root), cwd.starts_with(&root));
    let binaries = env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let server_id = format!("srv_{}", Uuid::new_v4().simple());
    let mut paths = ServicePaths::new(&server_id).unwrap();
    paths.executable = binaries.join("treer-agent-server.exe");
    paths.host_executable = binaries.join("treer-agent-host.exe");
    assert!(
        paths.executable.is_file() && paths.host_executable.is_file(),
        "build sibling binaries first"
    );
    let config = ServiceConfig {
        proxy: "http://127.0.0.1:9/".into(),
        workspace: "windows-test".into(),
        server_id: server_id.clone(),
        machine_token: "fixture-machine".into(),
        operator_credential: "fixture-operator".into(),
        root: root.clone(),
        listen: allocate_loopback_address().unwrap().to_string(),
        host_socket: host_socket_path(&server_id).unwrap(),
        install_hostname: current_hostname().unwrap(),
        network_mode: None,
        service_manager: ServiceManager::Detached,
        service_fallback_reason: None,
    };
    struct Cleanup(ServicePaths, ServiceConfig);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            if std::thread::panicking() {
                let pid = self.0.state_dir.join(format!(
                    "agent-host-{}.pid",
                    component_key(&self.1.server_id)
                ));
                let identity = fs::read(pid)
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
                let alive = identity
                    .as_ref()
                    .and_then(|value| {
                        Some((value["pid"].as_u64()? as u32, value["created_at"].as_u64()?))
                    })
                    .is_some_and(|(pid, created)| {
                        treer_platform::process_created_at(pid).ok().flatten() == Some(created)
                    });
                let log = fs::read_to_string(self.0.state_dir.join(format!(
                    "agent-server-{}.log",
                    component_key(&self.1.server_id)
                )))
                .unwrap_or_default();
                eprintln!("windows-real-host failure host-alive={alive} pipe-active={} log-present={} frame-error={} client-disconnected={} supervisor-restart={} socket-server-stopped={}", host_is_running(&self.1.host_socket), !log.is_empty(), log.contains("host frame") || log.contains("failed to read host request"), log.contains("host client disconnected"), log.contains("hot restarting controller"), log.contains("host socket server stopped"));
            }
            let _ = platform::stop(&self.0, &self.1);
            let _ = fs::remove_dir_all(&self.0.state_dir);
        }
    }
    save_json(&config, &paths.config).unwrap();
    save_json(
        &HostDaemonConfig {
            socket_path: config.host_socket.clone(),
            controller_path: paths.executable.clone(),
            controller_config_path: paths.config.clone(),
            root,
        },
        &paths.host_config,
    )
    .unwrap();
    let fixture = Cleanup(paths, config);
    platform::register(&fixture.0, &fixture.1).unwrap();
    platform::start(&fixture.0, &fixture.1).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !host_is_running(&fixture.1.host_socket) {
        assert!(
            Instant::now() < deadline,
            "detached Host pipe did not become available"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let pid_file = fixture
        .0
        .state_dir
        .join(format!("agent-host-{}.pid", fixture.1.server_id));
    let host_identity = fs::read(&pid_file).unwrap();
    let (host, events) = HostClient::connect(&fixture.1.host_socket).await.unwrap();
    let sync = host.sync(Default::default()).await.unwrap();
    let (runtime, _) = crate::controller::ControllerRuntime::from_sync(
        host.clone(),
        sync,
        events,
        crate::controller::ControllerConfig {
            protected_paths: vec![fixture.0.config.clone()],
            workspace_id: fixture.1.workspace.clone(),
            server_id: fixture.1.server_id.clone(),
            agent_server_url: format!("http://{}", fixture.1.listen),
            proxy_public_url: fixture.1.proxy.clone(),
            treer_binary: None,
            interface_cache_path: fixture.1.root.join("fixture-interfaces.json"),
            startup_store_path: fixture.1.root.join("fixture-startup.json"),
            root: fixture.1.root.clone(),
        },
    )
    .unwrap();
    let old_epoch = windows_wait_controller_epoch(&fixture.1, None).await;
    assert!(
        runtime
            .exec_machine(treer_protocol::MachineExecRequest {
                cwd: "..".into(),
                command: "node.exe".into(),
                args: vec![],
                timeout_ms: 30_000,
            })
            .await
            .is_err_and(|error| error.code == "invalid_machine_path"),
        "parent traversal must still be rejected"
    );
    windows_check_process_paths(&runtime, &host, &fixture.0.executable, &fixture.1.workspace).await;
    windows_wait_controller_epoch(&fixture.1, Some(&old_epoch)).await;
    let spawn = HostSpawnRequest {
        process_id: "survivor".into(),
        command: "powershell.exe".into(),
        args: vec![
            "-NoProfile".into(),
            "-Command".into(),
            "Start-Sleep -Seconds 60".into(),
        ],
        cwd: ".".into(),
        env: Default::default(),
        rows: 24,
        cols: 80,
        metadata: "{}".into(),
    };
    let HostResponse::Process { process: before } = host
        .request(
            HostCommand::Spawn {
                request: spawn.clone(),
            },
            Some("first-spawn".into()),
        )
        .await
        .unwrap()
    else {
        panic!("expected process");
    };
    platform::start(&fixture.0, &fixture.1).unwrap();
    assert_eq!(
        fs::read(&pid_file).unwrap(),
        host_identity,
        "duplicate start preserves Host"
    );
    restart_controller_at(&fixture.0, &fixture.1.host_socket).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let HostResponse::Synced { processes, .. } = host.sync(Default::default()).await.unwrap()
    else {
        panic!("expected inventory");
    };
    let survivor = processes
        .iter()
        .find(|p| p.process_id == "survivor")
        .unwrap();
    assert_eq!(survivor.pid, before.pid);
    assert_eq!(survivor.stream_epoch, before.stream_epoch);
    assert!(survivor.running);
    let workload_created = treer_platform::process_created_at(before.pid.unwrap())
        .unwrap()
        .unwrap();
    platform::stop(&fixture.0, &fixture.1).unwrap();
    assert!(!host_is_running(&fixture.1.host_socket));
    let deadline = Instant::now() + Duration::from_secs(5);
    while treer_platform::process_created_at(before.pid.unwrap()).unwrap() == Some(workload_created)
    {
        assert!(
            Instant::now() < deadline,
            "Host exit did not clean up the workload"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    platform::start(&fixture.0, &fixture.1).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !host_is_running(&fixture.1.host_socket) {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let (recovered, _) = HostClient::connect(&fixture.1.host_socket).await.unwrap();
    let HostResponse::Process { process: after } = recovered
        .request(
            HostCommand::Spawn { request: spawn },
            Some("restored-spawn".into()),
        )
        .await
        .unwrap()
    else {
        panic!("expected restored process");
    };
    assert_eq!(after.process_id, before.process_id);
    assert_ne!(after.stream_epoch, before.stream_epoch);
    platform::stop(&fixture.0, &fixture.1).unwrap();
    platform::stop(&fixture.0, &fixture.1).unwrap();
}

#[test]
fn workspace_keys_are_safe_for_service_names() {
    assert_eq!(workspace_key("team one/alpha"), "team_one_alpha");
    assert_eq!(workspace_key("default"), "default");
}

#[cfg(windows)]
async fn windows_check_process_paths(
    runtime: &crate::controller::ControllerRuntime,
    host: &crate::host_client::HostClient,
    controller: &Path,
    workspace: &str,
) {
    use treer_host_protocol::HostResponse;
    let node_script = "const a=require('node:assert'); for(const k of ['SystemRoot','windir','USERPROFILE','USERNAME','APPDATA','LOCALAPPDATA','ProgramData','HOMEDRIVE','HOMEPATH']) a.ok(process.env[k],k); for(const k of ['TREER_MACHINE_TOKEN','TREER_OPERATOR_CREDENTIAL','TREER_ENROLLMENT_KEY','AGENT_UI_INSTANCE_ID']) a.ok(!process.env[k],k); console.log('node-os-ok')";
    // Real Host/Controller are production binaries: no test-only cache hook.
    // Exercise CLR/Console without Utility module auto-loading. The compensated
    // Controller unit fixture and unmodified win_native smoke retain cmdlet
    // coverage. PowerShell may append native account paths, so check the fixed
    // system directory remains present, not equality after runtime startup.
    let powershell_script = "if (!$PSVersionTable.PSVersion -or !$env:SystemRoot -or !$env:USERPROFILE) { exit 2 }; $expected=[IO.Path]::Combine($env:SystemRoot,'System32\\WindowsPowerShell\\v1.0\\Modules'); $present=$false; foreach($segment in $env:PSModulePath.Split(';')) { if([string]::Equals($segment,$expected,[StringComparison]::OrdinalIgnoreCase)) {$present=$true} }; if(!$present) {exit 3}; $bytes=[Array]::CreateInstance([byte],16); [Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($bytes); [Console]::Out.WriteLine('powershell-os-ok'); exit 0";
    let commands: Vec<(String, Vec<String>, &str)> = vec![
        (
            "node.exe".into(),
            vec!["-e".into(), node_script.into()],
            "node-os-ok",
        ),
        (
            "powershell.exe".into(),
            vec![
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
                powershell_script.into(),
            ],
            "powershell-os-ok",
        ),
        (
            "tasklist.exe".into(),
            vec!["/FI".into(), "PID eq 0".into(), "/NH".into()],
            "",
        ),
        (
            controller.display().to_string(),
            vec![
                "service".into(),
                "--workspace".into(),
                workspace.into(),
                "status".into(),
            ],
            "Manager: Detached",
        ),
    ];
    for (index, (command, args, expected)) in commands.iter().enumerate() {
        let output = runtime
            .exec_machine(treer_protocol::MachineExecRequest {
                cwd: ".".into(),
                command: command.clone(),
                args: args.clone(),
                timeout_ms: 30_000,
            })
            .await
            .unwrap();
        eprintln!(
            "windows-real-host exec-smoke={index} duration_ms={} timeout={} exit={:?}",
            output.duration_ms, output.timed_out, output.exit_code
        );
        assert!(
            !output.timed_out,
            "real Host exec smoke {index} timed out under the original 30s limit"
        );
        assert_eq!(
            output.exit_code,
            Some(0),
            "exec smoke {index}: {}",
            output.stderr
        );
        assert!(
            output.stdout.contains(expected),
            "exec smoke {index} missing completion marker"
        );
        for kind in ["command", "app"] {
            let id = format!("{kind}-smoke-{index}");
            runtime
                .create(
                    &id,
                    id.clone(),
                    format!("wlc_{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple()),
                    treer_protocol::CreateAgentRequest {
                        folder_id: None,
                        server_id: None,
                        kind: kind.into(),
                        name: id.clone(),
                        cwd: ".".into(),
                        args: std::iter::once(command.clone())
                            .chain(args.clone())
                            .collect(),
                        cols: 100,
                        rows: 24,
                        publish_ports: vec![],
                        recipe: None,
                    },
                )
                .await
                .unwrap();
            windows_wait_process(host, &id, expected).await;
        }
    }
    // This is a Host-owned App process, not Controller-owned machine exec:
    // killing/replacing the Controller cannot kill the caller or its reply.
    let id = "host-owned-controller-restarter";
    runtime
        .create(
            id,
            id.into(),
            format!("wlc_{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple()),
            treer_protocol::CreateAgentRequest {
                folder_id: None,
                server_id: None,
                kind: "app".into(),
                name: id.into(),
                cwd: ".".into(),
                args: vec![
                    controller.display().to_string(),
                    "service".into(),
                    "--workspace".into(),
                    workspace.into(),
                    "restart-controller".into(),
                ],
                cols: 100,
                rows: 24,
                publish_ports: vec![],
                recipe: None,
            },
        )
        .await
        .unwrap();
    windows_wait_process(host, id, "").await;
    let HostResponse::Synced { .. } = host.sync(Default::default()).await.unwrap() else {
        panic!("Host did not survive hot restart")
    };
    println!("windows process matrix completed: exec=4 command=4 app=4 restart=1");
}

#[cfg(windows)]
async fn windows_wait_controller_epoch(config: &ServiceConfig, previous: Option<&str>) -> String {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(epoch) = controller_epoch(config).await {
            if Some(epoch.as_str()) != previous {
                return epoch;
            }
        }
        assert!(
            Instant::now() < deadline,
            "isolated Controller did not expose a fresh epoch"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[test]
fn root_relocation_preserves_identity_and_rejects_live_hosts_or_partial_state() {
    #[cfg(unix)]
    let base = PathBuf::from("/tmp");
    #[cfg(windows)]
    let base = env::temp_dir();
    let directory = base.join(format!("trm-{}", Uuid::new_v4().simple()));
    treer_platform::secure_directory(&directory).unwrap();
    let old = directory.join("old");
    let new = directory.join("new");
    fs::create_dir(&old).unwrap();
    fs::create_dir(&new).unwrap();
    let paths = ServicePaths {
        executable: directory.join("controller"),
        host_executable: directory.join("host"),
        config: directory.join("controller.json"),
        host_config: directory.join("host.json"),
        state_dir: directory.clone(),
    };
    #[cfg(unix)]
    let socket = directory.join("host.sock");
    #[cfg(windows)]
    let socket = host_socket_path(&format!("srv_{}", Uuid::new_v4().simple())).unwrap();
    let config = ServiceConfig {
        proxy: "http://127.0.0.1:9/".into(),
        workspace: "root-test".into(),
        server_id: "srv_root_fixture".into(),
        machine_token: "synthetic-machine".into(),
        operator_credential: "synthetic-operator".into(),
        root: old.clone(),
        listen: "127.0.0.1:9".into(),
        host_socket: socket.clone(),
        install_hostname: "fixture".into(),
        network_mode: None,
        service_manager: ServiceManager::Foreground,
        service_fallback_reason: None,
    };
    config.save(&paths.config).unwrap();
    save_json(
        &HostDaemonConfig {
            socket_path: socket.clone(),
            controller_path: paths.executable.clone(),
            controller_config_path: paths.config.clone(),
            root: old.clone(),
        },
        &paths.host_config,
    )
    .unwrap();
    let original: serde_json::Value =
        serde_json::from_slice(&fs::read(&paths.config).unwrap()).unwrap();
    {
        let _runtime = tokio::runtime::Runtime::new().unwrap();
        let _entered = _runtime.enter();
        let listener = treer_platform::LocalListener::bind(&socket).unwrap();
        assert!(relocate_root_at(&paths, &config, &new).is_err());
        drop(listener);
    }
    #[cfg(unix)]
    fs::remove_file(&socket).unwrap();
    relocate_root_at(&paths, &config, &new).unwrap();
    let migrated = ServiceConfig::load(&paths.config).unwrap();
    require_root_alignment(&paths, &migrated).unwrap();
    let mut after: serde_json::Value =
        serde_json::from_slice(&fs::read(&paths.config).unwrap()).unwrap();
    after["root"] = original["root"].clone();
    assert_eq!(
        after, original,
        "only root may change, not identity or credentials"
    );
    let mut incomplete = migrated.clone();
    incomplete.root = old;
    incomplete.save(&paths.config).unwrap();
    assert!(require_root_alignment(&paths, &incomplete).is_err());
    relocate_root_at(&paths, &incomplete, &new).unwrap();
    require_root_alignment(&paths, &ServiceConfig::load(&paths.config).unwrap()).unwrap();
    fs::remove_dir_all(directory).unwrap();
}

#[cfg(windows)]
async fn windows_wait_process(host: &crate::host_client::HostClient, id: &str, expected: &str) {
    use treer_host_protocol::{HostCommand, HostResponse};
    let deadline = Instant::now() + Duration::from_secs(30);
    eprintln!("windows-real-host wait-process={id}");
    loop {
        let HostResponse::Synced { processes, .. } = host
            .sync(Default::default())
            .await
            .unwrap_or_else(|error| panic!("wait-process={id} sync failed: {error}"))
        else {
            panic!("expected inventory")
        };
        let process = processes.iter().find(|p| p.process_id == id).unwrap();
        let HostResponse::Output { replay } = host
            .request(
                HostCommand::Read {
                    process_id: id.into(),
                    cursor: None,
                },
                None,
            )
            .await
            .unwrap()
        else {
            panic!("expected output")
        };
        let output = replay
            .chunks
            .iter()
            .flat_map(|chunk| chunk.data.iter().copied())
            .collect::<Vec<_>>();
        if !process.running && String::from_utf8_lossy(&output).contains(expected) {
            assert_eq!(process.exit_code, Some(0), "{id} failed");
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{id} did not exit with its completion marker"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[test]
#[cfg(unix)]
fn identity_and_socket_paths_are_scoped_to_node_and_server() {
    let identity = machine_identity_path().expect("machine identity path");
    assert!(identity.ends_with("machine-identity.json"));
    assert!(identity
        .components()
        .any(|component| component.as_os_str() == "machines"));
    let hostname_key = node_key().unwrap();
    assert!(identity
        .components()
        .any(|component| component.as_os_str() == std::ffi::OsStr::new(&hostname_key)));

    let socket = host_socket_path("srv_0123456789abcdef").expect("host socket path");
    assert_eq!(
        socket.file_name().and_then(|name| name.to_str()),
        Some("h-bc326bb4e71ef28b.sock")
    );
    assert_eq!(
        socket.file_name(),
        host_socket_path("srv_0123456789abcdef")
            .expect("same server")
            .file_name()
    );
    assert_ne!(
        host_socket_path("srv_0123456789abcdef")
            .expect("first server")
            .file_name(),
        host_socket_path("srv_fedcba9876543210")
            .expect("second server")
            .file_name()
    );
    assert!(unix_path_byte_len(&socket) <= MAX_UNIX_SOCKET_PATH_BYTES);
}

#[test]
#[cfg(unix)]
fn host_socket_filename_is_a_stable_short_hash() {
    assert_eq!(
        host_socket_filename("srv_0123456789abcdef"),
        "h-bc326bb4e71ef28b.sock"
    );
    assert_eq!(
        host_socket_filename("srv_0123456789abcdef").len(),
        "h-0123456789abcdef.sock".len()
    );
}

#[test]
#[cfg(unix)]
fn host_socket_falls_back_when_the_runtime_directory_is_too_long() {
    let filename = host_socket_filename("srv_0123456789abcdef");
    let too_long =
        PathBuf::from(format!("/{}", "a".repeat(MAX_UNIX_SOCKET_PATH_BYTES))).join(&filename);
    let fitted = fit_unix_socket_path(too_long, &filename).expect("fallback");
    assert!(unix_path_byte_len(&fitted) <= MAX_UNIX_SOCKET_PATH_BYTES);
    assert_eq!(
        fitted.file_name().and_then(|name| name.to_str()),
        Some(filename.as_str())
    );
    assert!(fitted.starts_with("/tmp"));
}

#[cfg(unix)]
#[test]
fn runtime_base_requires_a_private_writable_directory_owned_by_the_user() {
    use std::os::unix::fs::PermissionsExt;

    let directory = env::temp_dir().join(format!(
        "treer-runtime-permissions-{}",
        Uuid::new_v4().simple()
    ));
    fs::create_dir(&directory).expect("temporary runtime directory");
    let uid = current_uid()
        .expect("current uid")
        .parse::<u32>()
        .expect("numeric uid");
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
        .expect("private permissions");
    assert!(runtime_base_is_private_and_writable(&directory, uid));

    fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).expect("public permissions");
    assert!(!runtime_base_is_private_and_writable(&directory, uid));

    fs::set_permissions(&directory, fs::Permissions::from_mode(0o777))
        .expect("writable public permissions");
    assert!(!runtime_base_is_private_and_writable(&directory, uid));

    fs::set_permissions(&directory, fs::Permissions::from_mode(0o500))
        .expect("read-only permissions");
    assert!(!runtime_base_is_private_and_writable(&directory, uid));
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o600))
        .expect("non-searchable permissions");
    assert!(!runtime_base_is_private_and_writable(&directory, uid));
    assert!(!runtime_base_is_private_and_writable(
        &directory.join("missing"),
        uid
    ));
    fs::remove_dir(&directory).expect("remove temporary runtime directory");
}

#[cfg(target_os = "linux")]
#[test]
fn linux_runtime_dir_falls_back_when_the_user_runtime_directory_is_missing() {
    use std::os::unix::fs::PermissionsExt;

    let directory = env::temp_dir().join(format!(
        "treer-runtime-selection-{}",
        Uuid::new_v4().simple()
    ));
    fs::create_dir(&directory).expect("temporary directory");
    let uid = current_uid().expect("current uid");
    let uid_number = uid.parse::<u32>().expect("numeric uid");
    assert_eq!(linux_user_runtime_dir(&directory, &uid, uid_number), None);

    let user_runtime = directory.join(&uid);
    fs::create_dir(&user_runtime).expect("user runtime directory");
    fs::set_permissions(&user_runtime, fs::Permissions::from_mode(0o700))
        .expect("private permissions");
    assert_eq!(
        linux_user_runtime_dir(&directory, &uid, uid_number),
        Some(user_runtime.join("treer"))
    );
    fs::remove_dir_all(&directory).expect("remove temporary directory");
}

#[cfg(unix)]
#[test]
fn fallback_runtime_directory_is_created_with_private_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let directory = env::temp_dir().join(format!(
        "treer-runtime-fallback-{}",
        Uuid::new_v4().simple()
    ));
    let runtime = directory.join("run");
    assert_eq!(
        prepare_private_runtime_dir(runtime.clone()).expect("prepare runtime directory"),
        runtime
    );
    assert_eq!(
        fs::metadata(&runtime)
            .expect("runtime metadata")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    fs::remove_dir_all(directory).expect("remove temporary directory");
}

#[cfg(unix)]
#[test]
fn unavailable_legacy_socket_is_migrated_but_a_live_socket_is_not() {
    let directory = PathBuf::from("/tmp").join(format!(
        "treer-runtime-migration-{}",
        Uuid::new_v4().simple()
    ));
    fs::create_dir(&directory).expect("temporary directory");
    let current = directory.join("current.sock");
    let preferred = directory.join("preferred.sock");
    assert!(should_migrate_host_socket(&current, &preferred));
    assert!(!should_migrate_host_socket(&preferred, &preferred));

    let listener = std::os::unix::net::UnixListener::bind(&current).expect("live Host socket");
    assert!(!should_migrate_host_socket(&current, &preferred));
    drop(listener);
    fs::remove_dir_all(directory).expect("remove temporary directory");
}

#[cfg(unix)]
struct ActivationFixture {
    paths: ServicePaths,
    config: ServiceConfig,
    health: tokio::task::JoinHandle<std::io::Result<()>>,
}

#[cfg(unix)]
impl ActivationFixture {
    async fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;

        let directory =
            PathBuf::from("/tmp").join(format!("treer-activation-{}", Uuid::new_v4().simple()));
        fs::create_dir(&directory).unwrap();
        let paths = ServicePaths {
            executable: directory.join("controller"),
            host_executable: directory.join("host"),
            config: directory.join("controller.json"),
            host_config: directory.join("host.json"),
            state_dir: directory.clone(),
        };
        // Record actual subprocess invocations. The fixture can simulate a
        // Host disappearing after the socket probe, before the restart request.
        fs::write(
            &paths.host_executable,
            r#"#!/bin/sh
directory=$(dirname "$0")
echo "$1" >> "$directory/invocations"
if [ "$1" = restart-controller ]; then
    if [ -f "$directory/disappear" ]; then
        rm "$directory/host.sock"
    elif [ -f "$directory/succeed" ]; then
        exit 0
    fi
    echo 'Error: simulated Host restart failure' >&2
    exit 1
fi
exec sleep 5
"#,
        )
        .unwrap();
        fs::set_permissions(&paths.host_executable, fs::Permissions::from_mode(0o755)).unwrap();
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let config = ServiceConfig {
            proxy: "https://treer.example/".into(),
            workspace: "default".into(),
            server_id: "srv_test".into(),
            machine_token: "srv_test.secret".into(),
            operator_credential: "op_test".into(),
            root: directory.clone(),
            listen: listener.local_addr().unwrap().to_string(),
            host_socket: directory.join("host.sock"),
            install_hostname: current_hostname().unwrap(),
            network_mode: None,
            service_manager: ServiceManager::Foreground,
            service_fallback_reason: None,
        };
        let app = Router::new().route(
            "/api/health",
            get(|| async {
                Json(json!({
                    "service": "treer-agent-server",
                    "workspace_id": "default",
                    "server_id": "srv_test",
                    "proxy_connected": true,
                }))
            }),
        );
        let health = tokio::spawn(async move { axum::serve(listener, app).await });
        Self {
            paths,
            config,
            health,
        }
    }

    async fn stop_foreground(&self, activation: ServiceActivation) {
        let ServiceActivation::Foreground(mut child) = activation else {
            panic!("expected a newly started foreground Host");
        };
        self.wait_for_run().await;
        child.kill().await.unwrap();
        child.wait().await.unwrap();
    }

    async fn wait_for_run(&self) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !self.invocations().ends_with("run\n") {
            assert!(Instant::now() < deadline, "Host did not start");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn invocations(&self) -> String {
        fs::read_to_string(self.paths.state_dir.join("invocations")).unwrap_or_default()
    }
}

#[cfg(unix)]
impl Drop for ActivationFixture {
    fn drop(&mut self) {
        self.health.abort();
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if self.config.service_manager == ServiceManager::Nohup {
            let _ = stop_nohup(&self.paths, &self.config, true);
        }
        fs::remove_dir_all(&self.paths.state_dir).unwrap();
    }
}

#[cfg(unix)]
#[tokio::test]
async fn service_canary_cold_activation_skips_restart_for_missing_and_stale_sockets() {
    for stale in [false, true] {
        let fixture = ActivationFixture::new().await;
        if stale {
            drop(std::os::unix::net::UnixListener::bind(&fixture.config.host_socket).unwrap());
        }
        let activation = activate_registered_service(&fixture.paths, &fixture.config)
            .await
            .unwrap();
        fixture.stop_foreground(activation).await;
        assert_eq!(fixture.invocations(), "run\n");
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tokio::test]
async fn service_canary_cold_nohup_activation_records_and_stops_host() {
    let mut fixture = ActivationFixture::new().await;
    fixture.config.service_manager = ServiceManager::Nohup;
    assert!(matches!(
        activate_registered_service(&fixture.paths, &fixture.config)
            .await
            .unwrap(),
        ServiceActivation::Managed
    ));
    fixture.wait_for_run().await;
    let process = read_nohup_process(&fixture.paths, &fixture.config)
        .unwrap()
        .expect("recorded Host PID");
    assert!(nohup_process_is_current(&process).unwrap());
    assert_eq!(fixture.invocations(), "run\n");
    stop_nohup(&fixture.paths, &fixture.config, true).unwrap();
    assert!(!nohup_pid_path(&fixture.paths, &fixture.config).exists());
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tokio::test]
#[ignore = "requires cargo build -p treer-agent-host immediately before this isolated real-Host test"]
async fn service_canary_real_host_activation_preserves_workload_pid_and_pty() {
    use crate::host_client::HostClient;
    use std::os::unix::fs::PermissionsExt;
    use treer_host_protocol::{HostCommand, HostResponse, HostSpawnRequest};

    let mut fixture = ActivationFixture::new().await;
    fixture.config.service_manager = ServiceManager::Nohup;
    fixture.paths.host_executable = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target"))
        .join("debug/treer-agent-host");
    assert!(fixture.paths.host_executable.is_file(), "build Host first");
    fs::write(&fixture.paths.executable, "#!/bin/sh\nexec sleep 120\n").unwrap();
    fs::set_permissions(&fixture.paths.executable, fs::Permissions::from_mode(0o755)).unwrap();
    save_json(
        &HostDaemonConfig {
            socket_path: fixture.config.host_socket.clone(),
            controller_path: fixture.paths.executable.clone(),
            controller_config_path: fixture.paths.config.clone(),
            root: fixture.config.root.clone(),
        },
        &fixture.paths.host_config,
    )
    .unwrap();
    activate_registered_service(&fixture.paths, &fixture.config)
        .await
        .unwrap();
    let host_pid = read_nohup_process(&fixture.paths, &fixture.config)
        .unwrap()
        .unwrap()
        .pid;
    let deadline = Instant::now() + Duration::from_secs(5);
    let (host, _events) = loop {
        if let Ok(connection) = HostClient::connect(&fixture.config.host_socket).await {
            break connection;
        }
        assert!(Instant::now() < deadline, "Host socket did not appear");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let HostResponse::Process { process: before } = host
        .request(
            HostCommand::Spawn {
                request: HostSpawnRequest {
                    process_id: "survivor".into(),
                    command: "/bin/sh".into(),
                    args: vec!["-c".into(), "exec sleep 120".into()],
                    cwd: ".".into(),
                    env: Default::default(),
                    cols: 80,
                    rows: 24,
                    metadata: "{}".into(),
                },
            },
            Some("create-survivor".into()),
        )
        .await
        .unwrap()
    else {
        panic!("expected workload process");
    };
    for _ in 0..2 {
        activate_registered_service(&fixture.paths, &fixture.config)
            .await
            .unwrap();
        assert_eq!(
            read_nohup_process(&fixture.paths, &fixture.config)
                .unwrap()
                .unwrap()
                .pid,
            host_pid
        );
        let HostResponse::Synced { processes, .. } = host.sync(Default::default()).await.unwrap()
        else {
            panic!("expected Host inventory");
        };
        assert_eq!(processes.len(), 1);
        let after = &processes[0];
        assert!(after.running);
        assert_eq!(after.pid, before.pid);
        assert_eq!(after.stream_epoch, before.stream_epoch);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn service_canary_live_host_only_restarts_controller_on_repeated_activation() {
    let fixture = ActivationFixture::new().await;
    let listener = std::os::unix::net::UnixListener::bind(&fixture.config.host_socket).unwrap();
    fs::write(fixture.paths.state_dir.join("succeed"), "").unwrap();
    for _ in 0..2 {
        assert!(matches!(
            activate_registered_service(&fixture.paths, &fixture.config)
                .await
                .unwrap(),
            ServiceActivation::Managed
        ));
        // Drain the probe connection, keeping the same Host listener alive.
        listener.accept().unwrap();
    }
    assert_eq!(
        fixture.invocations(),
        "restart-controller\nrestart-controller\n"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn service_canary_host_exit_during_restart_recovers_with_a_cold_start() {
    let fixture = ActivationFixture::new().await;
    let _listener = std::os::unix::net::UnixListener::bind(&fixture.config.host_socket).unwrap();
    fs::write(fixture.paths.state_dir.join("disappear"), "").unwrap();
    let activation = activate_registered_service(&fixture.paths, &fixture.config)
        .await
        .unwrap();
    fixture.stop_foreground(activation).await;
    assert_eq!(fixture.invocations(), "restart-controller\nrun\n");
}

#[cfg(unix)]
#[tokio::test]
async fn service_canary_live_foreground_restart_failure_keeps_host_and_diagnostic() {
    let fixture = ActivationFixture::new().await;
    let listener = std::os::unix::net::UnixListener::bind(&fixture.config.host_socket).unwrap();
    let error = match activate_registered_service(&fixture.paths, &fixture.config).await {
        Err(error) => error,
        Ok(_) => panic!("expected restart failure"),
    };
    assert!(format!("{error:#}").contains("simulated Host restart failure"));
    assert_eq!(fixture.invocations(), "restart-controller\n");
    assert!(host_is_running(&fixture.config.host_socket));
    drop(listener);

    fs::remove_file(&fixture.paths.host_executable).unwrap();
    let error = match activate_registered_service(&fixture.paths, &fixture.config).await {
        Err(error) => error,
        Ok(_) => panic!("expected missing executable failure"),
    };
    assert!(format!("{error:#}").contains("failed to start foreground Host"));
}

#[test]
fn automatic_address_uses_the_loopback_port_range() {
    let address = allocate_loopback_address().expect("allocate local API address");
    assert!(address.ip().is_loopback());
    assert!(address.port() >= FIRST_AUTOMATIC_PORT);
}

#[tokio::test]
async fn local_api_identity_distinguishes_the_installed_controller() {
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind health server");
    let address = listener.local_addr().expect("health server address");
    let app = Router::new()
        .route(
            "/api/health",
            get(|| async {
                Json(json!({
                    "status": "ok",
                    "service": "treer-agent-server",
                    "workspace_id": "default",
                    "server_id": "srv_test",
                    "controller_epoch": "epoch-test",
                    "proxy_connected": true,
                    "connection_state": "online",
                }))
            }),
        )
        .route(
            "/api/agents",
            get(|| async { Json(json!({ "agents": [] })) }),
        );
    let server = tokio::spawn(async move { axum::serve(listener, app).await });

    assert!(local_api_matches(&address, "default", "srv_test").await);
    assert!(!local_api_matches(&address, "other", "srv_test").await);
    assert!(!local_api_matches(&address, "default", "srv_other").await);
    let config = ServiceConfig {
        proxy: "https://treer.example/".to_string(),
        workspace: "default".to_string(),
        server_id: "srv_test".to_string(),
        machine_token: "srv_test.secret".to_string(),
        operator_credential: "op_test".to_string(),
        root: PathBuf::from("/tmp"),
        listen: address.to_string(),
        host_socket: PathBuf::from("/tmp/host.sock"),
        install_hostname: current_hostname().expect("local hostname"),
        network_mode: None,
        service_manager: default_service_manager(),
        service_fallback_reason: None,
    };
    assert_eq!(
        controller_epoch(&config).await.as_deref(),
        Some("epoch-test")
    );
    wait_for_controller_and_proxy(&config)
        .await
        .expect("Controller and Proxy readiness");
    let occupant = occupying_controller(address)
        .await
        .expect("occupying Controller");
    assert_eq!(occupant.server_id, "srv_test");
    assert_eq!(occupant.workspace_id, "default");

    server.abort();
}

#[cfg(target_os = "linux")]
#[test]
fn automatic_service_mode_uses_nohup_without_probing_systemd() {
    let automatic = select_linux_service_manager(ServiceMode::Auto, || {
        panic!("automatic nohup mode must not probe systemd")
    })
    .expect("automatic nohup selection");
    assert_eq!(automatic.manager, ServiceManager::Nohup);
    assert_eq!(automatic.fallback_reason, None);

    let explicit = select_linux_service_manager(ServiceMode::Systemd, || {
        bail!("Failed to connect to bus: No such file or directory")
    })
    .expect_err("explicit systemd must fail");
    assert!(explicit.to_string().contains("user manager is unavailable"));

    let foreground = select_linux_service_manager(ServiceMode::Foreground, || {
        panic!("foreground mode must not probe systemd")
    })
    .expect("foreground selection");
    assert_eq!(foreground.manager, ServiceManager::Foreground);

    let nohup = select_linux_service_manager(ServiceMode::Nohup, || {
        panic!("explicit nohup mode must not probe systemd")
    })
    .expect("nohup selection");
    assert_eq!(nohup.manager, ServiceManager::Nohup);
}

#[cfg(target_os = "linux")]
#[test]
fn service_canary_healthy_systemd_user_manager_is_selected() {
    use std::os::unix::fs::PermissionsExt;

    let selection = select_linux_service_manager(ServiceMode::Systemd, || Ok(()))
        .expect("explicit systemd selection");
    assert_eq!(selection.manager, ServiceManager::SystemdUser);
    assert_eq!(selection.fallback_reason, None);

    let directory = std::env::temp_dir().join(format!(
        "treer-systemd-canary-{}",
        uuid::Uuid::new_v4().simple()
    ));
    fs::create_dir(&directory).expect("create canary directory");
    let log = directory.join("systemctl.log");
    let systemctl = directory.join("systemctl");
    fs::write(
        &systemctl,
        format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n", log.display()),
    )
    .expect("write fake systemctl");
    fs::set_permissions(&systemctl, fs::Permissions::from_mode(0o755))
        .expect("make fake systemctl executable");
    let unit = directory.join("treer-agent-server-srv_canary.service");
    let paths = ServicePaths::new("srv_canary").expect("service paths");
    let config = ServiceConfig {
        proxy: "https://treer.example/".to_string(),
        workspace: "canary".to_string(),
        server_id: "srv_canary".to_string(),
        machine_token: "srv_canary.secret".to_string(),
        operator_credential: "op_canary".to_string(),
        root: PathBuf::from("/tmp"),
        listen: "127.0.0.1:8790".to_string(),
        host_socket: directory.join("host.sock"),
        install_hostname: current_hostname().expect("hostname"),
        network_mode: None,
        service_manager: selection.manager,
        service_fallback_reason: selection.fallback_reason,
    };
    platform::register_systemd_with(systemctl.as_os_str(), &paths, &config, &unit)
        .expect("register systemd unit");
    let unit_contents = fs::read_to_string(&unit).expect("read generated unit");
    assert!(unit_contents.contains("Restart=always"));
    let calls = fs::read_to_string(log).expect("read systemctl calls");
    assert!(calls.contains("--user daemon-reload"));
    assert!(calls.contains("--user enable treer-agent-server-srv_canary.service"));

    fs::remove_dir_all(directory).expect("remove canary directory");
}

#[cfg(target_os = "linux")]
#[test]
fn service_canary_auto_ignores_a_missing_user_bus() {
    let selection = select_linux_service_manager(ServiceMode::Auto, || {
        panic!("auto mode must not inspect the user bus")
    })
    .expect("automatic nohup selection");
    assert_eq!(selection.manager, ServiceManager::Nohup);
    assert_eq!(selection.fallback_reason, None);
}

#[cfg(target_os = "linux")]
#[test]
fn nohup_stop_discards_a_reused_pid_without_signaling_it() {
    let (directory, paths, config) = nohup_test_service("stale-pid");
    let pid_path = nohup_pid_path(&paths, &config);
    save_json(
        &NohupProcess {
            pid: std::process::id(),
            started_at: "not the current process start time".to_string(),
        },
        &pid_path,
    )
    .expect("write stale PID record");

    stop_nohup(&paths, &config, true).expect("discard stale PID record");
    assert!(!pid_path.exists());

    fs::remove_dir_all(directory).expect("remove nohup state directory");
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn nohup_launcher_detaches_and_stops_the_recorded_process() {
    use std::os::unix::fs::PermissionsExt;

    let (directory, mut paths, config) = nohup_test_service("lifecycle");
    let fake_nohup = directory.join("fake nohup");
    let fake_host = directory.join("fake host");
    fs::write(&fake_nohup, "#!/bin/sh\nexec \"$@\"\n").expect("write fake nohup");
    fs::write(
        &fake_host,
        "#!/bin/sh\ntrap 'exit 0' TERM INT\nwhile :; do sleep 1; done\n",
    )
    .expect("write fake Host");
    fs::set_permissions(&fake_nohup, fs::Permissions::from_mode(0o755))
        .expect("make fake nohup executable");
    fs::set_permissions(&fake_host, fs::Permissions::from_mode(0o755))
        .expect("make fake Host executable");
    paths.host_executable = fake_host;

    start_nohup_with(fake_nohup.as_os_str(), &paths, &config).expect("start detached fake Host");
    let process = read_nohup_process(&paths, &config)
        .expect("read PID record")
        .expect("PID record");
    assert!(nohup_process_is_current(&process).expect("inspect fake Host"));
    // An own process group keeps the Host alive when the caller's group is
    // reaped, as launchd does when a job that ran `service restart` exits.
    let process_group = |pid: u32| {
        let output = Command::new("ps")
            .args(["-o", "pgid=", "-p", &pid.to_string()])
            .output()
            .expect("read process group");
        String::from_utf8(output.stdout)
            .expect("UTF-8 process group")
            .trim()
            .parse::<u32>()
            .expect("numeric process group")
    };
    assert_ne!(
        process_group(process.pid),
        process_group(std::process::id())
    );
    stop_nohup(&paths, &config, true).expect("stop detached fake Host");
    assert!(!nohup_pid_path(&paths, &config).exists());

    fs::remove_dir_all(directory).expect("remove nohup lifecycle directory");
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn nohup_test_service(label: &str) -> (PathBuf, ServicePaths, ServiceConfig) {
    let directory = std::env::temp_dir().join(format!(
        "treer-nohup-{label}-{}",
        uuid::Uuid::new_v4().simple()
    ));
    fs::create_dir(&directory).expect("create nohup test directory");
    let paths = ServicePaths {
        executable: directory.join("treer-agent-server"),
        host_executable: directory.join("treer-agent-host"),
        config: directory.join("controller.json"),
        host_config: directory.join("host config.json"),
        state_dir: directory.clone(),
    };
    let config = ServiceConfig {
        proxy: "https://treer.example/".to_string(),
        workspace: "default".to_string(),
        server_id: format!("srv_nohup_{label}"),
        machine_token: "srv_nohup.secret".to_string(),
        operator_credential: "op_nohup".to_string(),
        root: directory.clone(),
        listen: "127.0.0.1:8790".to_string(),
        host_socket: directory.join("host socket.sock"),
        install_hostname: current_hostname().expect("hostname"),
        network_mode: None,
        service_manager: ServiceManager::Nohup,
        service_fallback_reason: None,
    };
    (directory, paths, config)
}

#[cfg(target_os = "linux")]
#[test]
fn service_canary_explicit_systemd_failure_then_repairs_partial_unit() {
    use std::os::unix::fs::PermissionsExt;

    let explicit = select_linux_service_manager(ServiceMode::Systemd, || {
        bail!("Failed to connect to bus: No medium found")
    })
    .expect_err("explicit systemd must fail before enrollment");
    assert!(explicit.to_string().contains("user manager is unavailable"));

    let directory = std::env::temp_dir().join(format!(
        "treer-service-canary-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let unit = directory.join("treer-agent-server-test.service");
    let wants = directory
        .join("default.target.wants")
        .join("treer-agent-server-test.service");
    fs::create_dir_all(wants.parent().expect("wants parent")).expect("create canary directories");
    fs::write(&unit, "partial unit").expect("write partial unit");
    fs::write(&wants, "partial enablement").expect("write partial wants entry");
    let systemctl = directory.join("systemctl");
    fs::write(&systemctl, "#!/bin/sh\nexit 1\n").expect("write fake systemctl");
    fs::set_permissions(&systemctl, fs::Permissions::from_mode(0o755))
        .expect("make fake systemctl executable");

    platform::cleanup_systemd_registration_with(
        systemctl.as_os_str(),
        &unit,
        &wants,
        "treer-agent-server-test.service",
        false,
    )
    .expect("repair inactive partial registration without a user bus");
    assert!(!unit.exists());
    assert!(!wants.exists());

    fs::write(&unit, "active unit").expect("rewrite active unit");
    let error = platform::cleanup_systemd_registration_with(
        systemctl.as_os_str(),
        &unit,
        &wants,
        "treer-agent-server-test.service",
        true,
    )
    .expect_err("an active Host must not be orphaned when systemctl is unavailable");
    assert!(error
        .to_string()
        .contains("cannot safely switch supervision modes"));
    assert!(unit.exists());

    fs::remove_dir_all(directory).expect("remove canary directory");
}

#[cfg(target_os = "linux")]
#[test]
fn systemd_probe_preserves_the_actionable_bus_error() {
    use std::os::unix::fs::PermissionsExt;

    let directory = std::env::temp_dir().join(format!(
        "treer-fake-systemctl-{}",
        uuid::Uuid::new_v4().simple()
    ));
    fs::create_dir(&directory).expect("create fake systemctl directory");
    let executable = directory.join("systemctl");
    fs::write(
        &executable,
        b"#!/bin/sh\necho 'Failed to connect to bus: No such file or directory' >&2\nexit 1\n",
    )
    .expect("write fake systemctl");
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
        .expect("make fake systemctl executable");

    let error =
        probe_systemd_user_with(executable.as_os_str()).expect_err("unavailable fake user manager");
    assert!(error
        .to_string()
        .contains("Failed to connect to bus: No such file or directory"));

    fs::remove_file(executable).expect("remove fake systemctl");
    fs::remove_dir(directory).expect("remove fake systemctl directory");
}

#[test]
fn legacy_service_config_defaults_to_the_native_manager() {
    let config: ServiceConfig = serde_json::from_value(json!({
        "proxy": "https://treer.example/",
        "workspace": "default",
        "server_id": "srv_test",
        "machine_token": "token",
        "operator_credential": "operator",
        "root": "/tmp",
        "listen": "127.0.0.1:8790",
        "host_socket": "/tmp/host.sock",
        "install_hostname": "builder"
    }))
    .expect("legacy configuration");
    assert_eq!(config.service_manager, default_service_manager());
    assert_eq!(config.service_fallback_reason, None);
    assert_eq!(config.network_mode, None);
}

#[test]
fn native_experimental_network_mode_round_trips_in_service_config() {
    let encoded = serde_json::to_string(&NetworkMode::NativeExperimental)
        .expect("serialize native network mode");
    assert_eq!(encoded, "\"native-experimental\"");
    assert_eq!(
        serde_json::from_str::<NetworkMode>(&encoded).expect("deserialize native network mode"),
        NetworkMode::NativeExperimental
    );
    assert_eq!(
        NetworkMode::NativeExperimental.to_string(),
        "native-experimental"
    );
}

#[test]
fn update_platforms_match_release_artifact_names() {
    assert_eq!(
        artifact_platform("linux", "x86_64").unwrap(),
        "linux-x86_64"
    );
    assert_eq!(
        artifact_platform("linux", "aarch64").unwrap(),
        "linux-aarch64"
    );
    assert_eq!(
        artifact_platform("macos", "x86_64").unwrap(),
        "darwin-x86_64"
    );
    assert_eq!(
        artifact_platform("macos", "aarch64").unwrap(),
        "darwin-aarch64"
    );
    assert!(artifact_platform("windows", "x86_64").is_err());
}

#[test]
fn update_distributor_urls_preserve_the_directory_prefix() {
    let base = crate::release_update::normalize_base_url(
        Url::parse("https://treer.example/releases-root").unwrap(),
    )
    .unwrap();
    assert_eq!(
        base.join("channels/stable.json").unwrap().as_str(),
        "https://treer.example/releases-root/channels/stable.json"
    );
}

#[test]
fn update_distributor_requires_https_outside_loopback() {
    for source in [
        "file:///tmp/release",
        "http://releases.example/",
        "https://user:secret@releases.example/",
    ] {
        assert!(crate::release_update::normalize_base_url(Url::parse(source).unwrap()).is_err());
    }
}

#[test]
fn managed_agent_health_uses_the_injected_host_route() {
    let config = ServiceConfig {
        proxy: "https://treer.example/".to_string(),
        workspace: "default".to_string(),
        server_id: "srv_current".to_string(),
        machine_token: "srv_current.secret".to_string(),
        operator_credential: "op_test".to_string(),
        root: PathBuf::from("/tmp"),
        listen: "127.0.0.1:8790".to_string(),
        host_socket: PathBuf::from("/tmp/host.sock"),
        install_hostname: current_hostname().expect("local hostname"),
        network_mode: None,
        service_manager: default_service_manager(),
        service_fallback_reason: None,
    };

    assert_eq!(
        controller_health_url_for(
            &config,
            Some("srv_current"),
            Some("http://192.0.2.1:8790/ignored?old=true"),
        )
        .unwrap()
        .as_str(),
        "http://192.0.2.1:8790/api/health"
    );
    assert_eq!(
        controller_health_url_for(&config, Some("srv_other"), Some("http://192.0.2.1:9999/"),)
            .unwrap()
            .as_str(),
        "http://127.0.0.1:8790/api/health"
    );
}

#[test]
fn managed_agent_update_activates_only_its_controller() {
    let make_service = |server_id: &str| {
        let config = ServiceConfig {
            proxy: "https://treer.example/".to_string(),
            workspace: format!("workspace-{server_id}"),
            server_id: server_id.to_string(),
            machine_token: format!("{server_id}.secret"),
            operator_credential: "op_test".to_string(),
            root: PathBuf::from("/tmp"),
            listen: "127.0.0.1:8790".to_string(),
            host_socket: PathBuf::from(format!("/tmp/{server_id}.sock")),
            install_hostname: current_hostname().expect("local hostname"),
            network_mode: None,
            service_manager: default_service_manager(),
            service_fallback_reason: None,
        };
        (ServicePaths::new(server_id).expect("service paths"), config)
    };
    let services = vec![make_service("srv_a"), make_service("srv_b")];

    let managed = activation_services_for(&services, Some("srv_b")).unwrap();
    assert_eq!(managed.len(), 1);
    assert_eq!(managed[0].1.server_id, "srv_b");

    let host = activation_services_for(&services, None).unwrap();
    assert_eq!(
        host.iter()
            .map(|(_, config)| config.server_id.as_str())
            .collect::<Vec<_>>(),
        ["srv_a", "srv_b"]
    );
}

#[cfg(unix)]
#[test]
fn staged_executable_is_validated_and_atomically_installed() {
    use std::os::unix::fs::PermissionsExt;

    let directory = std::env::temp_dir().join(format!(
        "treer-update-stage-{}",
        uuid::Uuid::new_v4().simple()
    ));
    fs::create_dir(&directory).expect("create update test directory");
    let destination = directory.join("treer-test");
    let bytes = b"#!/bin/sh\nexit 0\n";
    let staged = stage_executable(&destination, bytes, "update", true).expect("stage executable");
    staged.install(&destination).expect("install executable");

    assert_eq!(fs::read(&destination).expect("read executable"), bytes);
    let mode = fs::metadata(&destination)
        .expect("executable metadata")
        .permissions()
        .mode()
        & 0o777;
    // Respect the installing user's umask (e.g. 022 => 0755, 077 => 0700).
    // Validation already executed it; never require broader public access.
    assert_eq!(mode & 0o700, 0o700);
    assert_eq!(mode & !0o755, 0);
    fs::remove_file(&destination).expect("remove executable");
    fs::remove_dir(&directory).expect("remove update test directory");
}

#[cfg(unix)]
#[test]
fn staged_executable_supports_restrictive_umask_in_an_isolated_process() {
    for mask in ["022", "077"] {
        let status = Command::new("sh")
            .arg("-c")
            .arg("umask \"$1\"; exec \"$2\" --exact service::tests::staged_executable_is_validated_and_atomically_installed")
            .arg("treer-umask-test")
            .arg(mask)
            .arg(env::current_exe().unwrap())
            .status()
            .expect("run isolated staging test");
        assert!(status.success(), "staging failed with umask {mask}");
    }
}

#[cfg(unix)]
#[test]
fn executable_validation_retries_only_busy_errors_within_the_deadline() {
    let mut attempts = 0;
    let result = retry_busy_executable(
        || {
            attempts += 1;
            if attempts < 3 {
                Err(std::io::Error::from_raw_os_error(libc::ETXTBSY))
            } else {
                Ok(())
            }
        },
        Instant::now() + Duration::from_secs(1),
    );
    assert!(result.is_ok());
    assert_eq!(attempts, 3);
    for errno in [libc::ETXTBSY, libc::EACCES, libc::ENOEXEC] {
        let mut attempts = 0;
        let error = retry_busy_executable::<()>(
            || {
                attempts += 1;
                Err(std::io::Error::from_raw_os_error(errno))
            },
            Instant::now()
                + if errno == libc::ETXTBSY {
                    Duration::ZERO
                } else {
                    Duration::from_secs(1)
                },
        )
        .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(errno));
        assert_eq!(attempts, 1);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn executable_validation_waits_for_a_real_writable_handle_to_close() {
    let directory = env::temp_dir().join(format!("treer-busy-executable-{}", Uuid::new_v4()));
    fs::create_dir(&directory).unwrap();
    let destination = directory.join("test-executable");
    let staged = stage_executable(&destination, b"#!/bin/sh\nexit 0\n", "test", false).unwrap();
    let writable = fs::OpenOptions::new()
        .write(true)
        .open(&staged.path)
        .unwrap();
    let error = Command::new(&staged.path)
        .arg("--help")
        .spawn()
        .unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::ETXTBSY));
    let closer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        drop(writable);
    });
    validate_executable(&staged.path, "test-executable").unwrap();
    closer.join().unwrap();
    drop(staged);
    fs::remove_dir(directory).unwrap();
}

#[test]
fn systemd_unit_quotes_paths_and_restarts() {
    let unit = systemd_unit(
        Path::new("/home/test user/bin/treer-agent-server"),
        Path::new("/home/test%user/config.json"),
        "team one",
        "build-node-1",
    );
    assert!(unit.contains("Restart=always"));
    assert!(unit.contains("ConditionHost=build-node-1"));
    assert!(unit.contains("\"/home/test user/bin/treer-agent-server\" run"));
    assert!(unit.contains("test%%user"));
}

#[test]
fn launchd_plist_escapes_values_and_keeps_process_alive() {
    let plist = launchd_plist(
        Path::new("/Users/a&b/treer-agent-server"),
        Path::new("/Users/a&b/config.json"),
        "dev.treer.test",
        Path::new("/tmp/out.log"),
        Path::new("/tmp/error.log"),
    );
    assert!(plist.contains("<key>KeepAlive</key>"));
    assert!(plist.contains("/Users/a&amp;b/treer-agent-server"));
    assert!(plist.contains("<string>run</string>"));
}

#[test]
fn launchd_start_enables_a_disabled_agent_before_bootstrap() {
    let target = "gui/502/dev.treer.agent-server.srv_94cc4ceea1dc4d8eaadc4d9b6fbe2958";
    let plist = Path::new(
            "/Users/mac/Library/LaunchAgents/dev.treer.agent-server.srv_94cc4ceea1dc4d8eaadc4d9b6fbe2958.plist",
        );
    let steps = launchd_start_steps(false, "gui/502", target, plist);
    assert_eq!(
        steps,
        vec![
            vec!["enable".to_string(), target.to_string()],
            vec![
                "bootstrap".to_string(),
                "gui/502".to_string(),
                plist.to_string_lossy().into_owned(),
            ],
        ]
    );
}

#[test]
fn launchd_start_enables_before_kickstart_when_already_loaded() {
    let target = "gui/502/dev.treer.agent-server.srv_94cc4ceea1dc4d8eaadc4d9b6fbe2958";
    let steps = launchd_start_steps(true, "gui/502", target, Path::new("/tmp/unused.plist"));
    assert_eq!(
        steps,
        vec![
            vec!["enable".to_string(), target.to_string()],
            vec!["kickstart".to_string(), target.to_string()],
        ]
    );
}

#[cfg(unix)]
#[test]
fn atomic_config_files_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let path = std::env::temp_dir().join(format!(
        "treer-config-permissions-{}.json",
        uuid::Uuid::new_v4().simple()
    ));
    write_atomic(&path, b"secret").expect("write configuration");
    let mode = std::fs::metadata(&path)
        .expect("configuration metadata")
        .permissions()
        .mode()
        & 0o777;
    std::fs::remove_file(&path).expect("remove test configuration");
    assert_eq!(mode, 0o600);
}

#[test]
fn machine_identity_round_trips_without_hardware_identifiers() {
    let directory = std::env::temp_dir().join(format!(
        "treer-machine-identity-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let path = directory.join("machine-identity.json");
    let identity = MachineIdentity::new("Build machine".to_string());
    identity.save(&path).expect("save machine identity");
    let loaded = MachineIdentity::load(&path).expect("load machine identity");
    assert_eq!(loaded, identity);
    let encoded = fs::read_to_string(&path).expect("read machine identity");
    assert!(!encoded.contains("mac_address"));
    assert!(!encoded.contains("hardware_address"));
    fs::remove_dir_all(directory).expect("remove identity directory");
}

#[test]
fn service_commands_list_or_select_local_installs() {
    assert_eq!(
        classify_service_request(None, true, &["ws_a", "ws_b"]),
        ServiceRequestClass::Inventory
    );
    assert_eq!(
        classify_service_request(None, false, &["ws_a"]),
        ServiceRequestClass::Unique
    );
    assert_eq!(
        classify_service_request(None, false, &["ws_a", "ws_b"]),
        ServiceRequestClass::Ambiguous
    );
    assert_eq!(
        classify_service_request(None, false, &[]),
        ServiceRequestClass::NoneInstalled
    );
    assert_eq!(
        classify_service_request(Some("default"), false, &["ws_a"]),
        ServiceRequestClass::Missing("default".to_string())
    );
    assert_eq!(
        classify_service_request(Some("ws_a"), false, &["ws_a", "ws_b"]),
        ServiceRequestClass::Selected
    );
    assert_eq!(
        classify_service_request(Some("default"), false, &["default"]),
        ServiceRequestClass::Selected
    );
}

#[test]
fn service_table_includes_workspace_and_listen() {
    let config = ServiceConfig {
        proxy: "https://treer.example/".to_string(),
        workspace: "ws_a39c30b35d6043918353e321cdd8ce96".to_string(),
        server_id: "srv_94cc4cee0123456789abcdef01234567".to_string(),
        machine_token: "token".to_string(),
        operator_credential: "operator".to_string(),
        root: PathBuf::from("/tmp"),
        listen: "127.0.0.1:8794".to_string(),
        host_socket: PathBuf::from("/tmp/host.sock"),
        install_hostname: "Mac.home.com".to_string(),
        network_mode: None,
        service_manager: default_service_manager(),
        service_fallback_reason: None,
    };
    let table = format_service_table("Mac.home.com", &[&config]);
    assert!(table.contains("ws_a39c30b35d6043918353e321cdd8ce96"));
    assert!(table.contains("127.0.0.1:8794"));
    assert!(!table.contains("pass --workspace"));
    let ambiguous = format_ambiguous_services("Mac.home.com", &[config]);
    assert!(ambiguous.contains("pass --workspace <workspace_id>"));
}
