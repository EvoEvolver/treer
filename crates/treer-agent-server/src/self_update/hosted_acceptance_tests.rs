//! Actual hosted-only process fixture. Never selected by an ordinary test run.
use super::*;
use futures_util::StreamExt;
use treer_platform::process_image::ProcessImage;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    current: wire::Identity,
    target: wire::Target,
    sums_sha256: String,
    host_sha256: String,
}

fn preflight_socket_path(root: &std::path::Path, operation_id: &str) -> Result<()> {
    super::super::job_id(operation_id)?;
    std::os::unix::net::SocketAddr::from_pathname(
        root.join("state/self-update/jobs")
            .join(operation_id)
            .join("handoff.sock"),
    )?;
    Ok(())
}

#[test]
fn socket_geometry_is_checked_without_creating_job_or_process() {
    let id = "01234567-89ab-4def-8123-456789abcdef";
    assert!(preflight_socket_path(std::path::Path::new("/h/tn-0123456789abcdef"), id).is_ok());
    assert!(
        preflight_socket_path(std::path::Path::new(&format!("/h/{}", "x".repeat(150))), id)
            .is_err()
    );
    assert!(
        preflight_socket_path(std::path::Path::new("/h/tn-0123456789abcdef"), "../other").is_err()
    );
}

fn read_json<T: serde::de::DeserializeOwned>(
    directory: &PrivateDirectory,
    name: &str,
) -> Result<T> {
    let mut bytes = Vec::new();
    directory
        .read_file(name)?
        .take(65537)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 65536 {
        bail!("hosted_update_input_invalid");
    }
    Ok(serde_json::from_slice(&bytes)?)
}
fn write_json(directory: &PrivateDirectory, name: &str, data: &serde_json::Value) -> Result<()> {
    let mut file = directory.create_file(name)?;
    file.write_all(&serde_json::to_vec_pretty(data)?)?;
    file.sync_all()?;
    directory.sync()?;
    Ok(())
}
fn clean_command(program: &std::path::Path, root: &std::path::Path) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    command
        .env_clear()
        .env(
            "HOME",
            nix::unistd::User::from_uid(nix::unistd::geteuid())
                .unwrap()
                .unwrap()
                .dir,
        )
        .env("XDG_CONFIG_HOME", root.join("xdg"))
        .env("TREER_STATE_DIR", root.join("state"))
        .env("TREER_ACCEPTANCE_ROOT", root)
        .env("TREER_DISPOSABLE_RUNNER", "1")
        .env("GITHUB_ACTIONS", "true")
        .env("RUNNER_ENVIRONMENT", "github-hosted")
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    command
}

#[derive(Clone, Default)]
struct SyntheticProxy {
    server: std::sync::Arc<std::sync::Mutex<Option<treer_protocol::ServerInfo>>>,
    profiles:
        std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, serde_json::Value>>>,
}
async fn profile_api(
    axum::extract::State(state): axum::extract::State<SyntheticProxy>,
    request: axum::extract::Request,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    if request
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        != Some("Bearer synthetic-native-machine")
    {
        return axum::http::StatusCode::UNAUTHORIZED.into_response();
    }
    let path = request.uri().path().to_owned();
    let method = request.method().clone();
    if path == "/agent/workspaces/ws_native_acceptance/snapshot"
        && method == axum::http::Method::GET
    {
        return axum::Json(serde_json::json!({"revision":1,"workspace":{"workspace_id":"ws_native_acceptance","name":"native acceptance","created_at":"2026-01-01T00:00:00Z"},
            "servers":[state.server.lock().unwrap().clone().unwrap()],"agents":[]})).into_response();
    }
    let Some(suffix) = path.strip_prefix("/agent/workspaces/ws_native_acceptance/launch-profiles")
    else {
        return axum::http::StatusCode::NOT_FOUND.into_response();
    };
    let name = percent_encoding::percent_decode_str(suffix.strip_prefix('/').unwrap_or(""))
        .decode_utf8()
        .unwrap()
        .into_owned();
    let bytes = axum::body::to_bytes(request.into_body(), 64 * 1024)
        .await
        .unwrap();
    let mut profiles = state.profiles.lock().unwrap();
    if method == axum::http::Method::GET {
        if name.is_empty() {
            return axum::Json(serde_json::json!(profiles.values().collect::<Vec<_>>()))
                .into_response();
        }
        if let Some(profile) = profiles.get(&name) {
            return axum::Json(profile.clone()).into_response();
        }
    } else if method == axum::http::Method::POST && name.is_empty() {
        let input: treer_protocol::CreateAgentLaunchProfileRequest =
            serde_json::from_slice(&bytes).unwrap();
        let profile = serde_json::json!({"profile_id":input.name,"workspace_id":"ws_native_acceptance","name":input.name,
            "description":input.description,"cwd":input.cwd,"command":input.command,"args":input.args,
            "created_at":chrono::Utc::now(),"updated_at":chrono::Utc::now(),"created_by":"synthetic","updated_by":"synthetic"});
        assert!(profiles.insert(input.name, profile.clone()).is_none());
        return axum::Json(profile).into_response();
    } else if method == axum::http::Method::PATCH {
        if let Some(profile) = profiles.get_mut(&name) {
            let update: treer_protocol::UpdateAgentLaunchProfileRequest =
                serde_json::from_slice(&bytes).unwrap();
            for (key, value) in serde_json::to_value(update).unwrap().as_object().unwrap() {
                profile[key] = value.clone();
            }
            return axum::Json(profile.clone()).into_response();
        }
    } else if method == axum::http::Method::DELETE && profiles.remove(&name).is_some() {
        return axum::Json(serde_json::json!({"ok":true})).into_response();
    }
    (axum::http::StatusCode::NOT_FOUND, axum::Json(serde_json::json!({"error":{"code":"launch_profile_not_found","message":"synthetic profile absent"}}))).into_response()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "explicit hosted disposable effective-account actual native update; never local/enrolled"]
async fn hosted_complete_native_fixture() {
    run_native_fixture(false).await;
}

#[path = "hosted_manager_tests.rs"]
mod manager;

#[path = "hosted_proxy_bridge_tests.rs"]
mod proxy_bridge;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "paired hosted private-account native worker plus real Proxy HTTP/PG; never enrolled"]
async fn hosted_proxy_database_native_fixture() {
    run_native_fixture_mode(false, true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "fresh hosted real Proxy websocket/HTTP/PG and feature Controller only"]
async fn hosted_controller_dispatch_native_fixture() {
    run_fixture(false, false, true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "explicit hosted disposable systemd account only; never local/enrolled"]
async fn hosted_systemd_native_fixture() {
    run_native_fixture(true).await;
}

async fn run_native_fixture(systemd: bool) {
    run_native_fixture_mode(systemd, false).await;
}

async fn run_native_fixture_mode(systemd: bool, bridge: bool) {
    run_fixture(systemd, bridge, false).await;
}

async fn run_fixture(systemd: bool, bridge: bool, websocket: bool) {
    assert!(
        !systemd || !bridge,
        "this increment does not repeat manager acceptance"
    );
    let root = root().expect("hosted effective-account eligibility required");
    let directory = PrivateDirectory::open(&root).unwrap();
    let input: Input = read_json(&directory, "acceptance-input.json").unwrap();
    input.current.validate().unwrap();
    input.target.validate().unwrap();
    if bridge {
        assert!(cfg!(target_os = "linux"));
        assert_eq!(input.target.platform, wire::Platform::LinuxX86_64);
        assert!(std::env::var_os("TREER_OPERATOR_CREDENTIAL").is_none());
    }
    assert!(!crate::controller_update::capability_available());
    let bin = directory.open_child("bin").unwrap();
    let target = directory.open_child("artifacts").unwrap();
    for (files, name, pin) in [
        (&bin, "treer-agent-server", &input.current.controller_sha256),
        (&bin, "treer", &input.current.cli_sha256),
        (&bin, "treer-agent-host", &input.host_sha256),
        (
            &target,
            "treer-agent-server",
            &input.target.identity.controller_sha256,
        ),
        (&target, "treer", &input.target.identity.cli_sha256),
    ] {
        assert_eq!(
            super::super::images::digest_file(&files.read_file(name).unwrap()).unwrap(),
            *pin
        );
    }
    assert_ne!(
        input.current.controller_sha256,
        input.target.identity.controller_sha256
    );
    assert_eq!(
        super::super::images::digest_file(&target.read_file("SHA256SUMS").unwrap()).unwrap(),
        input.sums_sha256
    );
    // Do not inherit or overwrite previous account launcher state. The hosted
    // job owner must allocate a fresh effective account/image for this fixture.
    let home = nix::unistd::User::from_uid(nix::unistd::geteuid())
        .unwrap()
        .unwrap()
        .dir;
    for name in [".local/bin/treer-agent", ".local/bin/treer-agent.json"] {
        assert!(
            std::fs::symlink_metadata(home.join(name))
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound),
            "preexisting launcher shim or uncertain account state"
        );
    }
    // Use the actual platform parser used by PrivateDirectory's production
    // handoff preflight, before config creation or any process launch. Do not
    // create the job directory: admission must still create it exclusively.
    let operation_id = uuid::Uuid::new_v4().to_string();
    preflight_socket_path(&root, &operation_id).unwrap();
    let xdg = directory.create_child("xdg").unwrap();
    let config = xdg
        .create_child("treer")
        .unwrap()
        .create_child("agent-servers")
        .unwrap();
    directory.create_child("state").unwrap();
    directory.create_child("workspace").unwrap();
    let hostname = nix::unistd::gethostname().unwrap().into_string().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_address = listener.local_addr().unwrap();
    let proxy_state = SyntheticProxy::default();
    let proxy = tokio::spawn(async move {
        if websocket {
            return;
        } // No synthetic socket/profile service in this mode.
        async fn socket(
            axum::extract::State(state): axum::extract::State<SyntheticProxy>,
            ws: axum::extract::WebSocketUpgrade,
        ) -> axum::response::Response {
            ws.on_upgrade(move |mut socket| async move {
                if let Some(Ok(axum::extract::ws::Message::Text(text))) = socket.next().await {
                    let treer_protocol::AgentServerMessage::Register {
                        server,
                        capabilities,
                        ..
                    } = serde_json::from_str(&text).unwrap()
                    else {
                        panic!("native fixture registration expected");
                    };
                    assert_eq!(server.server_id, "srv_native_acceptance");
                    *state.server.lock().unwrap() = Some(server);
                    assert!(!capabilities.iter().any(|value| value == wire::CAPABILITY));
                    socket
                        .send(axum::extract::ws::Message::Text(
                            serde_json::to_string(&treer_protocol::ProxyMessage::Registered {
                                protocol: treer_protocol::PROTOCOL_VERSION,
                                capabilities: vec![],
                                workspace_revision: 1,
                            })
                            .unwrap()
                            .into(),
                        ))
                        .await
                        .unwrap();
                    while let Some(Ok(message)) = socket.next().await {
                        if let axum::extract::ws::Message::Ping(bytes) = message {
                            if socket
                                .send(axum::extract::ws::Message::Pong(bytes))
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                }
            })
        }
        axum::serve(
            listener,
            axum::Router::new()
                .route("/agent/connect", axum::routing::get(socket))
                .fallback(profile_api)
                .with_state(proxy_state),
        )
        .await
        .unwrap();
    });
    let reserved = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = reserved.local_addr().unwrap();
    drop(reserved);
    let connection: serde_json::Value = if websocket {
        tokio::time::timeout(Duration::from_secs(90), async {
            loop {
                match read_json(&directory, "controller-proxy-ready.json") {
                    Ok(value) => break value,
                    Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
                }
            }
        })
        .await
        .expect("real Proxy readiness missing")
    } else {
        serde_json::json!({"proxy":format!("http://{proxy_address}"),"machine_token":"synthetic-native-machine"})
    };
    let url = url::Url::parse(connection["proxy"].as_str().unwrap()).unwrap();
    assert_eq!(url.scheme(), "http");
    assert_eq!(url.host_str(), Some("127.0.0.1"));
    assert!(
        url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
    );
    let mut controller_config = serde_json::json!({
        "proxy":connection["proxy"],"workspace":"ws_native_acceptance", "server_id":"srv_native_acceptance",
        "machine_token":connection["machine_token"], "operator_credential":"synthetic-native-operator",
        "root":root.join("workspace"), "listen":address.to_string(),"host_socket":root.join("host.sock"),
        "install_hostname":hostname,"network_mode":"plain"
    });
    if websocket {
        controller_config["hosted_native_dispatch"] = serde_json::json!(true);
    }
    write_json(
        &config,
        "srv_native_acceptance-controller.json",
        &controller_config,
    )
    .unwrap();
    write_json(&config, "srv_native_acceptance-host.json", &serde_json::json!({
        "socket_path":root.join("host.sock"),"controller_path":root.join("bin/treer-agent-server"),
        "controller_config_path":root.join("xdg/treer/agent-servers/srv_native_acceptance-controller.json"),"root":root.join("workspace")
    })).unwrap();
    let platform = input.target.platform;
    write_json(&config, "controller-update-catalog.json", &serde_json::json!({
        "schema_version":1,"revision":1,"accept_shared_binaries":true,"entries":[
            {"release_sequence":1,"target":{"target_id":"acceptance-old","identity":input.current,"platform":platform,"version_label":"acceptance old"},
             "sums_sha256":input.sums_sha256,"artifact_base":"https://native-acceptance.invalid/", "allow_pinned":true,"allow_artifact_origin":true},
            {"release_sequence":2,"target":input.target,"sums_sha256":input.sums_sha256,
             "artifact_base":"https://native-acceptance.invalid/", "allow_pinned":true,"allow_artifact_origin":true}
        ]
    })).unwrap();
    let mut binding = DispatchBinding {
        operation_id: operation_id.clone(),
        workspace_id: "ws_native_acceptance".into(),
        server_id: "srv_native_acceptance".into(),
        current: input.current.clone(),
        target: input.target.clone(),
        catalog_revision: 1,
        plan_digest: "0".repeat(64),
        created_at: chrono::Utc::now(),
    };
    binding.plan_digest = binding.expected_plan_digest().unwrap();
    if !bridge && !websocket {
        write_json(
            &directory,
            "binding.json",
            &serde_json::to_value(&binding).unwrap(),
        )
        .unwrap();
    }
    let mut host = manager::FixtureHost::start(&root, systemd).unwrap_or_else(|_| {
        if systemd {
            let _ = write_json(&directory,"native-manager-result.json",&serde_json::json!({
                "schema":1,"manager_lifecycle_acceptance":"unproven","stage":"unit_start",
                "cleanup":"inspect_owned_unit_intent_before_job_teardown","capability_enabled":false,
                "recovery_authority":false}));
        }
        panic!("fixture Host start failed; preserve owned unit intent, no retry")
    });
    let host_image = host.image().unwrap();
    let mut old_controller = None;
    let outcome: Result<()> = async {
        if super::super::images::digest_file(host_image.executable())? != input.host_sha256 {
            bail!("hosted_manager_host_image_mismatch");
        }
        let http = reqwest::Client::builder().no_proxy().build()?;
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if host.exited()? {
                    bail!("hosted_fixture_host_exited");
                }
                if let Ok(response) = http
                    .get(format!("http://{address}/api/health"))
                    .send()
                    .await
                {
                    if let Ok(value) = response.json::<serde_json::Value>().await {
                        if value["proxy_connected"] == true {
                            return Ok::<(), anyhow::Error>(());
                        }
                    }
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await??;
        if bridge {
            // Authenticate installed identities through the production local
            // inspector. Its ordinary capability must still be OFF.
            let inspection = crate::controller_update::inspect("ws_native_acceptance", "srv_native_acceptance")
                .await.map_err(|_| anyhow::anyhow!("bridge_native_inspection_failed"))?;
            assert!(!inspection.native_ready);
            proxy_bridge::publish(&directory, "proxy-native-inspection.json", &serde_json::to_value(inspection)?)?;
            binding = proxy_bridge::binding(&directory, &input).await?;
            preflight_socket_path(&root, &binding.operation_id)?;
            write_json(&directory, "binding.json", &serde_json::to_value(&binding)?)?;
        }
        if websocket {
            old_controller = Some(host.controller(&host_image, &input.current.controller_sha256)?);
            write_json(&directory,"controller-dispatch-custody-ready.json",&serde_json::json!({"old_controller_captured":true}))?;
            let deadline=tokio::time::Instant::now()+Duration::from_secs(420);
            // Evidence only: never transport a binding or invoke submit here.
            loop {
                host.verify(&host_image)?;
                if let Ok(actual)=read_json::<DispatchBinding>(&directory,"controller-dispatch-binding.json") {
                    binding=actual;
                    if let Ok(op)=super::super::web_adapter::operation(&binding.workspace_id,&binding.server_id,&binding.operation_id) {
                        if op.state==wire::State::Succeeded {
                            write_json(&directory,"native-acceptance-result.json",&serde_json::json!({"schema":1,"operation":op,"native_update_acceptance":"passed","capability_enabled":false,"dispatch_transport":"real_proxy_websocket","network_distributor_acceptance":"not_run"}))?;
                            break;
                        }
                    }
                }
                if tokio::time::Instant::now()>=deadline {bail!("real_controller_dispatch_timeout_no_retry");}
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        } else if systemd {
            old_controller = Some(host.controller(&host_image, &input.current.controller_sha256)?);
            host.release_submit(&directory)?;
            let deadline = tokio::time::Instant::now() + Duration::from_secs(420);
            loop {
                host.verify(&host_image)?;
                if directory.read_file("native-acceptance-result.json").is_ok() {
                    break;
                }
                if directory.read_file("native-submit-failure.json").is_ok() {
                    bail!("hosted_native_fixture_failed");
                }
                if tokio::time::Instant::now() >= deadline {
                    bail!("hosted_manager_completion_timeout");
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            host.wait_active().await?;
        } else {
            // The exact installed old image remains the submitter.
            let mut child = tokio::process::Command::from(clean_command(
                &root.join("bin/treer-agent-server"),
                &root,
            ));
            let submit = tokio::time::timeout(
                Duration::from_secs(420),
                child
                    .arg("--hosted-native-update-acceptance")
                    .kill_on_drop(true)
                    .status(),
            );
            tokio::pin!(submit);
            let status = loop {
                tokio::select! {
                    result = &mut submit => break result??,
                    _ = tokio::time::sleep(Duration::from_millis(50)), if bridge => {
                        // This is the real receipt projection, not a generated
                        // Accepted/Succeeded fixture response.
                        if let Ok(op) = super::super::web_adapter::operation(&binding.workspace_id, &binding.server_id, &binding.operation_id) {
                            proxy_bridge::publish_operation(&directory, &binding, &op)?;
                        }
                    }
                }
            };
            if !status.success() {
                bail!("hosted_native_fixture_failed");
            }
        }
        let result: serde_json::Value = read_json(&directory, "native-acceptance-result.json")?;
        if result["native_update_acceptance"] != "passed" || result["capability_enabled"] != false {
            bail!("hosted_native_fixture_incomplete");
        }
        if bridge {
            let op: wire::Operation = serde_json::from_value(result["operation"].clone())?;
            proxy_bridge::publish_operation(&directory, &binding, &op)?;
        }
        verify_installed_target(&root.join("bin"), &input.target.identity)?;
        host_image.verify_current()?;
        if let Some(old) = &old_controller {
            let new = host.controller(&host_image, &input.target.identity.controller_sha256)?;
            if old.creation_identity() == new.creation_identity() && old.pid() == new.pid() {
                bail!("hosted_manager_controller_not_replaced");
            }
            if old.verify_current().is_ok() {
                bail!("hosted_manager_old_controller_alive");
            }
            host.verify(&host_image)?;
            if !websocket {
            let worker: serde_json::Value = read_json(&directory, "native-manager-worker.json")?;
            if worker
                != serde_json::json!({"schema":1,"authenticated_worker_image":true,
                "same_unit_cgroup":true,"private_session":true,"recovery_authority":false})
            {
                bail!("hosted_manager_worker_unproven");
            }
            }
        }
        if websocket {
            tokio::time::timeout(Duration::from_secs(60),async {
                loop {
                    if directory.read_file("proxy-websocket-native-result.json").is_ok() {break;}
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }).await.map_err(|_|anyhow::anyhow!("proxy_final_observation_missing"))?;
        }
        Ok(())
    }
    .await;
    if outcome.is_err() {
        diagnostics::fixture_failure(&directory);
    }
    // Stop only the retained child or the acknowledged task-owned unit.
    let cleanup = host.stop(&host_image).await.unwrap_or(false) && !root.join("host.sock").exists();
    proxy.abort();
    let _ = proxy.await;
    write_json(&directory, "fixture-cleanup.json", &serde_json::json!({"host_reaped_and_socket_absent":cleanup,
        "launcher_account_state":"retained_for_job_teardown", "worker_and_claim_reconciliation":"inspect_durable_job_before_teardown",
        "manager_lifecycle_acceptance":if systemd && cleanup && outcome.is_ok() {"passed"} else if systemd {"unproven"} else {"not_run"},
        "native_result_available":outcome.is_ok()})).unwrap();
    if systemd {
        write_json(&directory, "native-manager-result.json", &serde_json::json!({
            "schema":1,"host_main_pid_identity_retained":outcome.is_ok(),
            "controller_replaced_and_healthy":outcome.is_ok(),
            "authenticated_worker_in_unit":outcome.is_ok(),
            "unit_stopped_and_cgroup_empty":cleanup,"observed_zero_manager_restarts":outcome.is_ok(),
            "capability_enabled":false,"manager_lifecycle_acceptance":if cleanup && outcome.is_ok() {"passed"} else {"unproven"},
            "scope":"transient_systemd_user_host_and_exec_start_post_submit",
            "production_service_install_and_reboot":"not_run"})).unwrap();
    }
    assert!(
        cleanup,
        "owned Host cleanup unconfirmed; preserve fixture for job teardown"
    );
    outcome.expect("actual native fixture did not complete; no automatic retry");
}

// Publication intentionally uses installation permissions (0755), not private
// input permissions. Authenticate the new images through retained install handles.
fn verify_installed_target(bin: &std::path::Path, target: &wire::Identity) -> Result<()> {
    let installation = treer_platform::installation_directory::InstallationDirectory::open(bin)?;
    for (name, digest) in [
        ("treer-agent-server", &target.controller_sha256),
        ("treer", &target.cli_sha256),
    ] {
        let path = bin.join(name);
        let image =
            super::super::verified_image::VerifiedImage::open(&installation, &path, digest)?;
        image.verify()?;
    }
    Ok(())
}

#[test]
fn installed_target_reader_accepts_publication_modes_and_rejects_unsafe_images() {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir()
        .canonicalize()
        .unwrap()
        .join(format!("treer-installed-reader-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let directory = PrivateDirectory::open(&root).unwrap();
    let mut target = wire::Identity {
        release_sha: "a".repeat(40),
        artifact_source_sha: "a".repeat(40),
        controller_sha256: String::new(),
        cli_sha256: String::new(),
    };
    for name in ["treer-agent-server", "treer"] {
        let mut file = directory.create_file(name).unwrap();
        file.write_all(name.as_bytes()).unwrap();
        let digest = super::super::images::digest_file(&file).unwrap();
        if name == "treer" {
            target.cli_sha256 = digest;
        } else {
            target.controller_sha256 = digest;
        }
        file.set_permissions(std::fs::Permissions::from_mode(0o755))
            .unwrap();
        assert!(directory.read_file(name).is_err());
    }
    assert!(verify_installed_target(&root, &target).is_ok());
    let mut wrong = target.clone();
    wrong.controller_sha256 = "0".repeat(64);
    assert!(verify_installed_target(&root, &wrong).is_err());
    for mode in [0o777, 0o600] {
        std::fs::set_permissions(root.join("treer"), std::fs::Permissions::from_mode(mode))
            .unwrap();
        assert!(verify_installed_target(&root, &target).is_err());
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn synthetic_profile_transport_keeps_encoded_name_readback_and_missing_code() {
    use axum::body::{to_bytes, Body};
    let state = SyntheticProxy::default();
    let call = |method, path: &str, body: serde_json::Value| {
        let request = axum::http::Request::builder()
            .method(method)
            .uri(path)
            .header("authorization", "Bearer synthetic-native-machine")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        profile_api(axum::extract::State(state.clone()), request)
    };
    let created = call(axum::http::Method::POST, "/agent/workspaces/ws_native_acceptance/launch-profiles",
        serde_json::json!({"name":"Codex + UI","description":"synthetic","cwd":".","command":"treer-agent","args":["--harness","codex"]})).await;
    assert_eq!(created.status(), axum::http::StatusCode::OK);
    let read = call(
        axum::http::Method::GET,
        "/agent/workspaces/ws_native_acceptance/launch-profiles/Codex%20%2B%20UI",
        serde_json::Value::Null,
    )
    .await;
    assert_eq!(read.status(), axum::http::StatusCode::OK);
    let value: serde_json::Value =
        serde_json::from_slice(&to_bytes(read.into_body(), 65536).await.unwrap()).unwrap();
    assert_eq!(value["command"], "treer-agent");
    assert_eq!(value["args"], serde_json::json!(["--harness", "codex"]));
    call(
        axum::http::Method::DELETE,
        "/agent/workspaces/ws_native_acceptance/launch-profiles/Codex%20%2B%20UI",
        serde_json::Value::Null,
    )
    .await;
    let missing = call(
        axum::http::Method::GET,
        "/agent/workspaces/ws_native_acceptance/launch-profiles/Codex%20%2B%20UI",
        serde_json::Value::Null,
    )
    .await;
    assert_eq!(missing.status(), axum::http::StatusCode::NOT_FOUND);
    let value: serde_json::Value =
        serde_json::from_slice(&to_bytes(missing.into_body(), 65536).await.unwrap()).unwrap();
    assert_eq!(value["error"]["code"], "launch_profile_not_found");
    let unauthorized = profile_api(
        axum::extract::State(state),
        axum::http::Request::new(Body::empty()),
    )
    .await;
    assert_eq!(unauthorized.status(), axum::http::StatusCode::UNAUTHORIZED);
}
