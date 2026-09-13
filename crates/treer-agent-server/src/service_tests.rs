use super::*;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;

#[test]
fn workspace_keys_are_safe_for_service_names() {
    assert_eq!(workspace_key("team one/alpha"), "team_one_alpha");
    assert_eq!(workspace_key("default"), "default");
}

#[test]
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

#[cfg(target_os = "linux")]
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
    stop_nohup(&paths, &config, true).expect("stop detached fake Host");
    assert!(!nohup_pid_path(&paths, &config).exists());

    fs::remove_dir_all(directory).expect("remove nohup lifecycle directory");
}

#[cfg(target_os = "linux")]
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
fn update_artifact_urls_are_relative_to_the_proxy_root() {
    let proxy = Url::parse("https://treer.example/").unwrap();
    assert_eq!(
        artifact_url(&proxy, "darwin-aarch64", "treer-agent-server")
            .unwrap()
            .as_str(),
        "https://treer.example/artifacts/darwin-aarch64/treer-agent-server"
    );
}

#[test]
fn explicit_update_proxy_overrides_the_installed_source() {
    let config = ServiceConfig {
        proxy: "https://stable.treer.example/".to_string(),
        workspace: "default".to_string(),
        server_id: "srv_test".to_string(),
        machine_token: "srv_test.secret".to_string(),
        operator_credential: "op_test".to_string(),
        root: PathBuf::from("/tmp"),
        listen: "127.0.0.1:8790".to_string(),
        host_socket: PathBuf::from("/tmp/host.sock"),
        install_hostname: current_hostname().expect("local hostname"),
        network_mode: None,
        service_manager: default_service_manager(),
        service_fallback_reason: None,
    };
    let explicit = Url::parse("https://canary.treer.example/").unwrap();

    assert_eq!(
        resolve_update_proxy(Some(explicit), &config)
            .unwrap()
            .as_str(),
        "https://canary.treer.example/"
    );
    assert_eq!(
        resolve_update_proxy(None, &config).unwrap().as_str(),
        "https://stable.treer.example/"
    );
    assert!(
        resolve_update_proxy(Some(Url::parse("file:///tmp/release").unwrap()), &config).is_err()
    );
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
    assert_eq!(
        fs::metadata(&destination)
            .expect("executable metadata")
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    fs::remove_file(&destination).expect("remove executable");
    fs::remove_dir(&directory).expect("remove update test directory");
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
