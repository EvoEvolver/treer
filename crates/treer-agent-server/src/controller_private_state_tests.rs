//! Per-Agent private state: derivation, fresh injection, retention, omission
//! on unsafe or in-cwd directories, tombstone-driven purge and the quota
//! ledger, driven through a fake Host that records spawns.
use super::*;
use crate::private_state::{QuotaLimits, ENVIRONMENT_VARIABLE as VARIABLE};
use std::future::IntoFuture;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use treer_host_protocol::{
    decode_message, encode_message, HostBuildInfo, HostMessage, HostRequest,
};

pub(crate) struct Fixture {
    pub(crate) root: PathBuf,
    bin: PathBuf,
    pub(crate) state: PathBuf,
    socket: PathBuf,
    sync: HostResponse,
    pub(crate) spawns: Arc<Mutex<Vec<HostSpawnRequest>>>,
    stops: Arc<Mutex<Vec<String>>>,
    host_task: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.host_task.abort();
        let _ = std::fs::remove_dir_all(&self.root);
        let _ = std::fs::remove_dir_all(&self.state);
    }
}

fn executable_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.into()
    }
}

/// A fake Host that answers Sync with its inventory, records every Spawn as
/// a running process and marks Stop targets exited. Accepts reconnects so a
/// replacement Controller can be built against the same inventory.
pub(crate) async fn fixture() -> Fixture {
    let id = &Uuid::new_v4().simple().to_string()[..12];
    let root = std::env::temp_dir().join(format!("trr-private-{id}"));
    let state = std::env::temp_dir().join(format!("trr-private-state-{id}"));
    std::fs::create_dir_all(root.join("repo")).unwrap();
    std::fs::create_dir_all(&state).unwrap();
    let root = std::fs::canonicalize(root).unwrap();
    let state = std::fs::canonicalize(state).unwrap();
    let bin = root.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let executable = bin.join(executable_name("agent-command"));
    std::fs::write(&executable, b"mock Host executable").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    #[cfg(unix)]
    let socket = root.join("host.sock");
    #[cfg(windows)]
    let socket = treer_platform::endpoint_name(
        &Uuid::new_v4().to_string(),
        &treer_platform::current_user_sid().unwrap(),
    );
    let mut listener = treer_platform::LocalListener::bind(&socket).unwrap();
    let sync = HostResponse::Synced {
        host_epoch: "private-state-host".into(),
        host_build: HostBuildInfo {
            version: "test".into(),
            git_commit: "test".into(),
        },
        processes: vec![],
        replay: vec![],
    };
    let host_sync = sync.clone();
    let spawns = Arc::new(Mutex::new(Vec::<HostSpawnRequest>::new()));
    let stops = Arc::new(Mutex::new(Vec::<String>::new()));
    let observed = spawns.clone();
    let observed_stops = stops.clone();
    let host_task = tokio::spawn(async move {
        let inventory = Arc::new(Mutex::new(Vec::<HostProcessInfo>::new()));
        loop {
            let Ok(mut stream) = listener.accept().await else {
                return;
            };
            let (inventory, host_sync, observed, observed_stops) = (
                inventory.clone(),
                host_sync.clone(),
                observed.clone(),
                observed_stops.clone(),
            );
            tokio::spawn(async move {
                while let Ok(length) = stream.read_u32().await {
                    let mut bytes = vec![0; length as usize];
                    stream.read_exact(&mut bytes).await.unwrap();
                    let request: HostRequest = decode_message(&bytes).unwrap();
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
                                processes: inventory.lock().unwrap().clone(),
                                replay,
                            }
                        }
                        HostCommand::Spawn { request } => {
                            observed.lock().unwrap().push(request.clone());
                            let now = Utc::now();
                            let metadata: AgentMetadata =
                                serde_json::from_str(&request.metadata).unwrap();
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
                            let mut processes = inventory.lock().unwrap();
                            processes.retain(|old| old.process_id != process.process_id);
                            processes.push(process.clone());
                            HostResponse::Process { process }
                        }
                        HostCommand::Stop { process_id } => {
                            observed_stops.lock().unwrap().push(process_id.clone());
                            let mut processes = inventory.lock().unwrap();
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
                        other => panic!("unexpected Host command {other:?}"),
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
        }
    });
    Fixture {
        root,
        bin,
        state,
        socket,
        sync,
        spawns,
        stops,
        host_task,
    }
}

impl Fixture {
    pub(crate) fn config(&self, server_id: &str, private_state_root: PathBuf) -> ControllerConfig {
        ControllerConfig {
            protected_paths: vec![],
            workspace_id: "workspace".into(),
            server_id: server_id.into(),
            agent_server_url: "http://127.0.0.1:1".into(),
            proxy_public_url: "https://proxy.example/".into(),
            treer_binary: None,
            interface_cache_path: self.root.join("interfaces.json"),
            startup_store_path: self.root.join("startup.json"),
            private_state_root,
            provider_cli_root: self.root.join("provider-cli"),
            root: self.root.clone(),
        }
    }

    pub(crate) async fn runtime(
        &self,
        config: ControllerConfig,
    ) -> Result<ControllerRuntime, ProtocolError> {
        let (host, events) = HostClient::connect(&self.socket).await.unwrap();
        ControllerRuntime::from_sync(host, self.sync.clone(), events, config).map(|(r, _)| r)
    }

    fn request(&self, cwd: &str) -> CreateAgentRequest {
        CreateAgentRequest {
            folder_id: None,
            server_id: None,
            kind: "command".into(),
            name: "target".into(),
            cwd: cwd.into(),
            args: vec![self
                .bin
                .join(executable_name("agent-command"))
                .to_str()
                .unwrap()
                .into()],
            cols: 80,
            rows: 24,
            publish_ports: vec![],
            recipe: None,
        }
    }

    fn spawn_env(&self, index: usize) -> BTreeMap<String, String> {
        self.spawns.lock().unwrap()[index].env.clone()
    }
}

pub(crate) async fn create(
    runtime: &ControllerRuntime,
    fixture: &Fixture,
    agent_id: &str,
    cwd: &str,
) -> String {
    let credential = new_workload_credential();
    runtime
        .create(
            &format!("create-{agent_id}"),
            agent_id.into(),
            credential.clone(),
            fixture.request(cwd),
        )
        .await
        .unwrap();
    credential
}

#[tokio::test]
async fn private_state_is_derived_injected_fresh_and_retained_across_restart() {
    let fixture = fixture().await;
    let private_root = fixture.state.join("agent-private");
    let runtime = fixture
        .runtime(fixture.config("srv_a", private_root.clone()))
        .await
        .unwrap();
    // The root exists before any Agent starts so it can be protected.
    assert!(private_root.join("srv_a").is_dir());
    create(&runtime, &fixture, "ag_one", "repo").await;
    let expected = private_root.join("srv_a").join("ag_one");
    let env = fixture.spawn_env(0);
    assert_eq!(env[VARIABLE], expected.to_str().unwrap());
    assert_eq!(env["TREER_AGENT_ID"], "ag_one");
    assert!(expected.is_dir());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for level in [&private_root, &private_root.join("srv_a"), &expected] {
            assert_eq!(
                std::fs::metadata(level).unwrap().permissions().mode() & 0o777,
                0o700,
                "{}",
                level.display()
            );
        }
    }
    // The directory is outside the workload cwd and never under the machine root.
    assert!(!expected.starts_with(&fixture.root));
    let report = runtime.private_state_report("ag_one").unwrap();
    assert_eq!(report.status, "available");
    assert_eq!(report.usage.unwrap().used_bytes, 0);

    // Stop keeps the directory and its content; restart injects the same path.
    std::fs::write(expected.join("attachments.json"), b"{}").unwrap();
    runtime.stop("stop", "ag_one").await.unwrap();
    assert!(expected.join("attachments.json").exists());
    runtime.start("restart", "ag_one", true).await.unwrap();
    assert_eq!(fixture.spawns.lock().unwrap().len(), 2);
    assert_eq!(fixture.spawn_env(1)[VARIABLE], expected.to_str().unwrap());
    assert_eq!(
        std::fs::read(expected.join("attachments.json")).unwrap(),
        b"{}"
    );

    // A replacement Controller over the same store derives the same path,
    // re-measures what is on disk and still finds the content: Controller
    // restart is not deletion. (The first runtime still holds the Agent's
    // workload proxy port, so recovery injection is covered by the restart
    // above, which also runs through spawn_startup.)
    drop(runtime);
    let replacement = fixture
        .runtime(fixture.config("srv_a", private_root.clone()))
        .await
        .unwrap();
    let report = replacement.private_state_report("ag_one").unwrap();
    assert_eq!(report.status, "available");
    assert_eq!(report.usage.unwrap().used_bytes, 2);
    assert!(expected.join("attachments.json").exists());
    assert_eq!(
        replacement.startup_validation_ids(),
        vec!["ag_one".to_string()]
    );
    assert_eq!(
        replacement
            .inner
            .private_state
            .as_ref()
            .unwrap()
            .agent_ids()
            .unwrap(),
        vec!["ag_one".to_string()]
    );
}

#[cfg(unix)]
#[tokio::test]
async fn an_unsafe_root_degrades_private_state_without_blocking_the_controller() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = fixture().await;
    let private_root = fixture.state.join("agent-private");
    std::fs::create_dir(&private_root).unwrap();
    std::fs::set_permissions(&private_root, std::fs::Permissions::from_mode(0o755)).unwrap();
    // A pre-existing group/world-readable root: the Controller still starts,
    // Agents still launch and recover, only the variable is omitted.
    let runtime = fixture
        .runtime(fixture.config("srv_a", private_root.clone()))
        .await
        .unwrap();
    create(&runtime, &fixture, "ag_one", "repo").await;
    let env = fixture.spawn_env(0);
    assert!(!env.contains_key(VARIABLE));
    assert_eq!(env["TREER_AGENT_ID"], "ag_one");
    runtime.stop("stop", "ag_one").await.unwrap();
    runtime.start("restart", "ag_one", true).await.unwrap();
    assert!(!fixture.spawn_env(1).contains_key(VARIABLE));
    let report = runtime.private_state_report("ag_one").unwrap();
    assert_eq!(report.status, "agent_private_state_unsafe");
    assert!(report.usage.is_none());
    assert_eq!(
        runtime.reserve_private_state("ag_one", 1).unwrap_err().code,
        "agent_private_state_unsafe"
    );
    assert_eq!(runtime.startup_validation_ids(), vec!["ag_one".to_string()]);
    let deferred = runtime
        .purge_deleted_private_state(&["ag_one".into()])
        .await;
    assert_eq!(
        deferred[0].1.clone().unwrap_err().code,
        "agent_private_state_unsafe"
    );
    assert!(
        !private_root.join("srv_a").exists(),
        "nothing is created below an unsafe root"
    );

    // A server level that cannot be listed degrades enumeration only.
    std::fs::set_permissions(&private_root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let server = private_root.join("srv_b");
    std::fs::create_dir(&server).unwrap();
    std::fs::set_permissions(&server, std::fs::Permissions::from_mode(0o300)).unwrap();
    let runtime = fixture
        .runtime(fixture.config("srv_b", private_root.clone()))
        .await
        .unwrap();
    create(&runtime, &fixture, "ag_two", "repo").await;
    assert_eq!(
        fixture.spawn_env(2)[VARIABLE],
        server.join("ag_two").to_str().unwrap()
    );
    std::fs::set_permissions(&server, std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn an_incomplete_inventory_pauses_reservations_until_a_full_recount_succeeds() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = fixture().await;
    let private_root = fixture.state.join("agent-private");
    // Another Agent already holds 80 bytes; the server level then becomes
    // unlistable (0300: traversable, not readable) before the Controller starts.
    let server = private_root.join("srv_a");
    let other = server.join("ag_other");
    std::fs::create_dir_all(&other).unwrap();
    for level in [&private_root, &server, &other] {
        std::fs::set_permissions(level, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::fs::write(other.join("blob"), vec![0; 80]).unwrap();
    std::fs::set_permissions(&server, std::fs::Permissions::from_mode(0o300)).unwrap();
    let runtime = fixture
        .runtime(fixture.config("srv_a", private_root.clone()))
        .await
        .unwrap();
    runtime
        .inner
        .private_state_ledger
        .lock()
        .unwrap()
        .set_limits(QuotaLimits {
            agent_bytes: 100,
            global_bytes: 100,
            reservation_bytes: 100,
            reservations_per_agent: 4,
            reservation_ttl: Duration::from_secs(60),
            free_space_floor_bytes: 0,
        });
    // The Agent still starts with its directory injected...
    create(&runtime, &fixture, "ag_a", "repo").await;
    assert_eq!(
        fixture.spawn_env(0)[VARIABLE],
        server.join("ag_a").to_str().unwrap()
    );
    // ...but unknown existing usage never reads as zero: no admission.
    assert_eq!(
        runtime.reserve_private_state("ag_a", 30).unwrap_err().code,
        "agent_private_state_unavailable"
    );
    assert_eq!(
        runtime.private_state_report("ag_a").unwrap_err().code,
        "agent_private_state_unavailable"
    );
    // Repairing the listing recovers the full inventory on the next call and
    // the global cap counts the other Agent's 80 bytes.
    std::fs::set_permissions(&server, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        runtime.reserve_private_state("ag_a", 30).unwrap_err().code,
        "private_state_global_quota_exceeded"
    );
    assert!(runtime.reserve_private_state("ag_a", 20).is_ok());
    assert_eq!(
        runtime
            .private_state_report("ag_a")
            .unwrap()
            .usage
            .unwrap()
            .available_bytes,
        0
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_non_unicode_private_root_is_omitted_without_failing_the_launch() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    let fixture = fixture().await;
    let private_root = fixture
        .state
        .join(OsString::from_vec(b"agent-private-\xff".to_vec()));
    let runtime = fixture
        .runtime(fixture.config("srv_a", private_root.clone()))
        .await
        .unwrap();
    create(&runtime, &fixture, "ag_one", "repo").await;
    let env = fixture.spawn_env(0);
    assert!(!env.contains_key(VARIABLE));
    assert_eq!(env["TREER_AGENT_ID"], "ag_one");
    assert_eq!(
        runtime.private_state_report("ag_one").unwrap().status,
        "agent_private_state_unavailable"
    );
    assert_eq!(
        runtime.reserve_private_state("ag_one", 1).unwrap_err().code,
        "agent_private_state_unavailable"
    );
}

/// Sealed joint-validation fixture for launcher-dev: the REAL local API router
/// and ControllerRuntime over a fake Host, a synthetic workload identity and a
/// temporary private root. Prints one JSON line and serves until the deadline.
/// Run: TREER_PRIVATE_STATE_FIXTURE_SECONDS=300 [TREER_PRIVATE_STATE_FIXTURE_DELETE_AFTER=120]
///      cargo test -p treer-agent-server private_state_joint_fixture -- --ignored --nocapture
#[ignore = "manual joint-validation fixture; prints connection details and serves for a while"]
#[tokio::test]
async fn private_state_joint_fixture() {
    let seconds: u64 = std::env::var("TREER_PRIVATE_STATE_FIXTURE_SECONDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(300);
    let delete_after: Option<u64> = std::env::var("TREER_PRIVATE_STATE_FIXTURE_DELETE_AFTER")
        .ok()
        .and_then(|value| value.parse().ok());
    let fixture = fixture().await;
    let private_root = fixture.state.join("agent-private");
    let runtime = fixture
        .runtime(fixture.config("srv_fixture", private_root.clone()))
        .await
        .unwrap();
    let credential = create(&runtime, &fixture, "ag_fixture", "repo").await;
    let state = crate::local_api::LocalApiState::new(
        url::Url::parse("http://127.0.0.1:1/").unwrap(),
        "workspace".into(),
        "srv_fixture".into(),
        None,
        Some(new_workload_credential()),
        treer_protocol::BuildInfo {
            version: "fixture".into(),
            git_commit: "fixture".into(),
        },
        runtime.clone(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(axum::serve(listener, crate::local_api::router(state)).into_future());
    println!(
        "{}",
        serde_json::json!({
            "fixture": "private_state_joint_fixture",
            "url": url,
            "agent_id": "ag_fixture",
            "server_id": "srv_fixture",
            "workload_credential": credential,
            "private_state_dir": fixture.spawn_env(0)[VARIABLE],
            "feature": treer_protocol::CAPABILITY_AGENT_PRIVATE_STATE,
            "serves_for_seconds": seconds,
            "delete_after_seconds": delete_after,
        })
    );
    let started = Instant::now();
    let mut purged = false;
    while started.elapsed() < Duration::from_secs(seconds) {
        tokio::time::sleep(Duration::from_millis(250)).await;
        if let Some(after) = delete_after {
            if !purged && started.elapsed() >= Duration::from_secs(after) {
                // Simulate the Proxy tombstone confirmation: stop, then purge.
                runtime.stop("fixture-stop", "ag_fixture").await.unwrap();
                let results = runtime
                    .purge_deleted_private_state(&["ag_fixture".into()])
                    .await;
                println!(
                    "{}",
                    serde_json::json!({
                        "event": "purged",
                        "result": format!("{:?}", results[0].1),
                        "directory_exists": private_root.join("srv_fixture").join("ag_fixture").exists(),
                    })
                );
                purged = true;
            }
        }
    }
    server.abort();
}

#[tokio::test]
async fn invalid_identities_never_become_paths() {
    let fixture = fixture().await;
    let private_root = fixture.state.join("agent-private");
    let error = fixture
        .runtime(fixture.config("srv/../a", private_root.clone()))
        .await
        .map(|_| ())
        .unwrap_err();
    assert_eq!(error.code, "agent_private_state_invalid_identity");
    let runtime = fixture
        .runtime(fixture.config("srv_a", private_root.clone()))
        .await
        .unwrap();
    let error = runtime
        .create(
            "create",
            "../escape".into(),
            new_workload_credential(),
            fixture.request("repo"),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "agent_private_state_invalid_identity");
    assert!(fixture.spawns.lock().unwrap().is_empty());
    assert!(!fixture.state.join("escape").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn unsafe_private_directory_is_omitted_and_the_agent_still_starts() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let fixture = fixture().await;
    let private_root = fixture.state.join("agent-private");
    let runtime = fixture
        .runtime(fixture.config("srv_a", private_root.clone()))
        .await
        .unwrap();
    let elsewhere = fixture.state.join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    symlink(&elsewhere, private_root.join("srv_a").join("ag_linked")).unwrap();
    create(&runtime, &fixture, "ag_linked", "repo").await;
    let open = private_root.join("srv_a").join("ag_open");
    std::fs::create_dir(&open).unwrap();
    std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o770)).unwrap();
    create(&runtime, &fixture, "ag_open", "repo").await;
    assert_eq!(
        fixture.spawns.lock().unwrap().len(),
        2,
        "the Agent itself starts"
    );
    for index in 0..2 {
        let env = fixture.spawn_env(index);
        assert!(
            !env.contains_key(VARIABLE),
            "no path to an unsafe directory"
        );
        assert!(env.contains_key("TREER_AGENT_ID"));
    }
    for agent_id in ["ag_linked", "ag_open"] {
        let report = runtime.private_state_report(agent_id).unwrap();
        assert_eq!(report.status, "agent_private_state_unsafe");
        assert!(report.usage.is_none());
        assert_eq!(
            runtime.reserve_private_state(agent_id, 1).unwrap_err().code,
            "agent_private_state_unsafe"
        );
    }
    assert!(
        elsewhere.read_dir().unwrap().next().is_none(),
        "nothing was written through the link"
    );
    // Repairing the mode makes the next start inject again.
    std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o700)).unwrap();
    runtime.start("restart", "ag_open", true).await.unwrap();
    assert_eq!(fixture.spawn_env(2)[VARIABLE], open.to_str().unwrap());
}

/// #133: a hidden state directory under a plain home is allowed; a repository
/// anywhere from the private directory up to the filesystem root refuses with
/// a distinct code and an actionable hint; the verdict is recomputed on every
/// start, restart and report.
#[tokio::test]
async fn hidden_private_state_under_a_plain_home_is_injected_until_a_repository_appears() {
    let fixture = fixture().await;
    let private_root = fixture
        .root
        .join(".local/state/treer")
        .join("agent-private");
    let runtime = fixture
        .runtime(fixture.config("srv_a", private_root.clone()))
        .await
        .unwrap();
    create(&runtime, &fixture, "ag_home", ".").await;
    let expected = private_root.join("srv_a").join("ag_home");
    assert_eq!(fixture.spawn_env(0)[VARIABLE], expected.to_str().unwrap());
    let report = runtime.private_state_report("ag_home").unwrap();
    assert_eq!((report.status, report.hint), ("available", None));
    // Defence in depth: ignore files in the agent-private root, private and
    // never rewritten.
    for name in [".gitignore", ".ignore"] {
        let path = private_root.join(name);
        assert_eq!(std::fs::read(&path).unwrap(), b"*\n", "{name}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::write(&path, b"custom\n").unwrap();
    }
    runtime.start("restart", "ag_home", true).await.unwrap();
    assert_eq!(
        std::fs::read(private_root.join(".gitignore")).unwrap(),
        b"custom\n"
    );
    assert_eq!(fixture.spawn_env(1)[VARIABLE], expected.to_str().unwrap());
    // git init in the home between two evaluations: the next report and the
    // next start refuse with the VCS code; reservations refuse too.
    std::fs::create_dir(fixture.root.join(".git")).unwrap();
    let report = runtime.private_state_report("ag_home").unwrap();
    assert_eq!(report.status, "agent_private_state_inside_vcs_worktree");
    let hint = report.hint.expect("actionable hint");
    assert!(hint.contains("version-controlled tree"), "{hint}");
    assert!(!hint.contains('/'), "hints never carry paths: {hint}");
    assert_eq!(
        runtime
            .reserve_private_state("ag_home", 1)
            .unwrap_err()
            .code,
        "agent_private_state_inside_vcs_worktree"
    );
    runtime.start("restart", "ag_home", true).await.unwrap();
    assert!(!fixture.spawn_env(2).contains_key(VARIABLE));
    std::fs::remove_dir(fixture.root.join(".git")).unwrap();
    // A `.git` FILE (worktree/submodule) and a symlinked marker count too.
    std::fs::write(fixture.root.join(".git"), b"gitdir: ../elsewhere").unwrap();
    assert_eq!(
        runtime.private_state_report("ag_home").unwrap().status,
        "agent_private_state_inside_vcs_worktree"
    );
    std::fs::remove_file(fixture.root.join(".git")).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            fixture.root.join("nowhere"),
            fixture.root.join(".local/.hg"),
        )
        .unwrap();
        assert_eq!(
            runtime.private_state_report("ag_home").unwrap().status,
            "agent_private_state_inside_vcs_worktree"
        );
        std::fs::remove_file(fixture.root.join(".local/.hg")).unwrap();
    }
    assert_eq!(
        runtime.private_state_report("ag_home").unwrap().status,
        "available"
    );
    // A cwd below a repository root: the marker sits at a cwd ANCESTOR.
    std::fs::create_dir_all(fixture.root.join("home")).unwrap();
    let nested_root = fixture.root.join("home/.local/state/treer/agent-private");
    let nested = fixture
        .runtime(fixture.config("srv_b", nested_root))
        .await
        .unwrap();
    create(&nested, &fixture, "ag_nested", "home").await;
    assert!(
        fixture.spawn_env(3).contains_key(VARIABLE),
        "no marker yet: allowed"
    );
    std::fs::create_dir(fixture.root.join(".jj")).unwrap();
    assert_eq!(
        nested.private_state_report("ag_nested").unwrap().status,
        "agent_private_state_inside_vcs_worktree"
    );
    std::fs::remove_dir(fixture.root.join(".jj")).unwrap();
    // The repository rule is independent of the cwd: an Agent whose cwd is a
    // sibling project still loses attachments while the home is a repository.
    create(&nested, &fixture, "ag_project", "repo").await;
    assert!(fixture.spawn_env(4).contains_key(VARIABLE));
    std::fs::create_dir(fixture.root.join(".git")).unwrap();
    assert_eq!(
        nested.private_state_report("ag_project").unwrap().status,
        "agent_private_state_inside_vcs_worktree"
    );
    std::fs::remove_dir(fixture.root.join(".git")).unwrap();
}

#[tokio::test]
async fn private_state_inside_the_workload_cwd_is_not_injected() {
    let fixture = fixture().await;
    // A machine root that contains the Treer state directory on a NON-hidden
    // path (root/state/...): the workload cwd "." contains the private
    // directory in a tree tools would scan, so it stays refused (#133 keeps
    // this; only a hidden, repository-free path is allowed).
    let private_root = fixture.root.join("state").join("agent-private");
    let runtime = fixture
        .runtime(fixture.config("srv_a", private_root.clone()))
        .await
        .unwrap();
    create(&runtime, &fixture, "ag_home", ".").await;
    let env = fixture.spawn_env(0);
    assert!(
        !env.contains_key(VARIABLE),
        "a directory inside cwd must not be advertised"
    );
    assert_eq!(env["TREER_AGENT_ID"], "ag_home");
    let report = runtime.private_state_report("ag_home").unwrap();
    assert_eq!(report.status, "agent_private_state_inside_cwd");
    assert!(report
        .hint
        .is_some_and(|hint| hint.contains("set the Agent's cwd") && !hint.contains('/')));
    assert_eq!(
        runtime
            .reserve_private_state("ag_home", 1)
            .unwrap_err()
            .code,
        "agent_private_state_inside_cwd"
    );
    // A sibling cwd that does not contain the directory still receives it.
    create(&runtime, &fixture, "ag_repo", "repo").await;
    assert_eq!(
        fixture.spawn_env(1)[VARIABLE],
        private_root.join("srv_a").join("ag_repo").to_str().unwrap()
    );
}

#[tokio::test]
async fn only_proxy_confirmed_deletions_purge_and_running_agents_are_stopped_first() {
    let fixture = fixture().await;
    let private_root = fixture.state.join("agent-private");
    let runtime = fixture
        .runtime(fixture.config("srv_a", private_root.clone()))
        .await
        .unwrap();
    let credential = create(&runtime, &fixture, "ag_gone", "repo").await;
    create(&runtime, &fixture, "ag_live", "repo").await;
    create(&runtime, &fixture, "ag_keep", "repo").await;
    let gone = private_root.join("srv_a").join("ag_gone");
    let live = private_root.join("srv_a").join("ag_live");
    let keep = private_root.join("srv_a").join("ag_keep");
    for directory in [&gone, &live, &keep] {
        std::fs::write(directory.join("state.json"), b"{}").unwrap();
    }
    // Another server's namespace and link targets are never touched.
    let other = private_root.join("srv_other").join("ag_gone");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("state.json"), b"{}").unwrap();
    #[cfg(unix)]
    {
        let outside = fixture.state.join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("keep"), b"keep").unwrap();
        std::os::unix::fs::symlink(&outside, gone.join("escape")).unwrap();
    }
    runtime.stop("stop", "ag_gone").await.unwrap();
    let proxy_port = runtime
        .inner
        .workload_proxies
        .ensure("ag_gone", &credential, None)
        .unwrap();
    assert!(
        tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, proxy_port))
            .await
            .is_ok()
    );
    // Stop, restart and an empty confirmation never purge anything.
    assert!(runtime.purge_deleted_private_state(&[]).await.is_empty());
    assert!(gone.join("state.json").exists());
    assert_eq!(
        runtime.startup_validation_ids(),
        vec!["ag_gone".to_string(), "ag_keep".into(), "ag_live".into()]
    );

    let results = runtime
        .purge_deleted_private_state(&[
            "ag_gone".into(),
            "ag_live".into(),
            "ag_absent".into(),
            "../bad".into(),
        ])
        .await;
    let outcome = |id: &str| {
        results
            .iter()
            .find(|(agent, _)| agent == id)
            .map(|(_, result)| result.clone())
            .unwrap()
    };
    assert_eq!(outcome("ag_gone"), Ok(true));
    assert_eq!(outcome("ag_absent"), Ok(false));
    assert_eq!(
        outcome("ag_live").unwrap_err().code,
        "agent_private_state_purge_deferred"
    );
    assert!(!results.iter().any(|(agent, _)| agent == "../bad"));
    assert!(!gone.exists());
    assert!(
        tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, proxy_port))
            .await
            .is_err()
    );
    assert!(runtime.authenticate_agent("ag_gone", &credential).is_err());
    assert!(
        runtime.get_startup("ag_gone").is_err(),
        "stale startup entry removed"
    );
    assert!(
        runtime.get("ag_gone").is_err(),
        "terminal registry entry removed"
    );
    assert!(runtime.inner.startup_store.is_denied("ag_gone").unwrap());
    assert!(
        live.join("state.json").exists(),
        "running Agents are not purged"
    );
    assert_eq!(
        fixture.stops.lock().unwrap().as_slice(),
        ["ag_gone", "ag_live"]
    );
    assert!(keep.join("state.json").exists());
    assert!(other.join("state.json").exists());
    #[cfg(unix)]
    assert!(fixture.state.join("outside").join("keep").exists());
    let report = runtime.private_state_report("ag_gone").unwrap();
    assert_eq!(report.status, "agent_private_state_unavailable");
    assert_eq!(
        runtime
            .reserve_private_state("ag_gone", 1)
            .unwrap_err()
            .code,
        "agent_private_state_unavailable"
    );

    // The next validation pass finishes the deferred purge once the Host
    // reports the process exited.
    let results = runtime
        .purge_deleted_private_state(&["ag_live".into()])
        .await;
    assert_eq!(results[0].1, Ok(true));
    assert!(!live.exists());
    assert_eq!(
        runtime.startup_validation_ids(),
        vec!["ag_keep".to_string()]
    );
    assert!(runtime.get_startup("ag_keep").is_ok());

    // Repeated recovery must not re-register retained, exited Host inventory.
    runtime.restore_runtime(&[]).await;
    assert!(runtime.get("ag_gone").is_err());
    assert!(runtime.get("ag_live").is_err());
    let mut sync = runtime.inner.host.sync(Default::default()).await.unwrap();
    if let HostResponse::Synced {
        processes, replay, ..
    } = &mut sync
    {
        for process in processes.iter().filter(|p| p.process_id != "ag_keep") {
            assert_eq!(
                runtime
                    .upsert_process(process.clone(), None)
                    .unwrap_err()
                    .code,
                "agent_identity_denied",
                "late Host inventory is fenced"
            );
            replay.push(HostOutputReplay {
                process_id: process.process_id.clone(),
                stream_epoch: process.stream_epoch.clone(),
                first_available_revision: 1,
                next_revision: 1,
                gap: false,
                chunks: vec![],
            });
        }
    }
    let (host, events) = HostClient::connect(&fixture.socket).await.unwrap();
    let (replacement, _) =
        ControllerRuntime::from_sync(host, sync, events, fixture.config("srv_a", private_root))
            .unwrap();
    assert!(replacement.get("ag_gone").is_err());
    assert!(replacement.get("ag_live").is_err());
    assert!(replacement.get_startup("ag_gone").is_err());
    assert_eq!(
        replacement
            .create(
                "late-create",
                "ag_gone".into(),
                new_workload_credential(),
                fixture.request("repo")
            )
            .await
            .unwrap_err()
            .code,
        "agent_identity_denied"
    );
}

#[tokio::test]
async fn quota_is_controller_measured_scoped_per_agent_and_recounted_after_restart() {
    let fixture = fixture().await;
    let private_root = fixture.state.join("agent-private");
    let runtime = fixture
        .runtime(fixture.config("srv_a", private_root.clone()))
        .await
        .unwrap();
    create(&runtime, &fixture, "ag_a", "repo").await;
    create(&runtime, &fixture, "ag_b", "repo").await;
    let limits = QuotaLimits {
        agent_bytes: 1000,
        global_bytes: 1300,
        reservation_bytes: 600,
        reservations_per_agent: 2,
        reservation_ttl: Duration::from_secs(60),
        free_space_floor_bytes: 0,
    };
    runtime
        .inner
        .private_state_ledger
        .lock()
        .unwrap()
        .set_limits(limits);
    let a = private_root.join("srv_a").join("ag_a");
    let b = private_root.join("srv_a").join("ag_b");

    // Reserve, write, commit: the Controller re-measures the whole directory.
    let reservation = runtime.reserve_private_state("ag_a", 500).unwrap();
    assert!(reservation.reservation_id.starts_with("psr_"));
    assert_eq!(
        runtime.reserve_private_state("ag_a", 501).unwrap_err().code,
        "private_state_agent_quota_exceeded"
    );
    std::fs::create_dir(a.join("images")).unwrap();
    std::fs::write(a.join("images").join("one.png"), vec![0; 400]).unwrap();
    std::fs::write(a.join("journal.json"), vec![0; 50]).unwrap();
    let usage = runtime
        .commit_private_state_reservation("ag_a", &reservation.reservation_id)
        .unwrap();
    assert_eq!(usage.used_bytes, 450, "journal and images both count");
    assert!(usage.reservation_ids.is_empty());
    assert_eq!(
        runtime
            .commit_private_state_reservation("ag_a", &reservation.reservation_id)
            .unwrap_err()
            .code,
        "private_state_reservation_expired",
        "a repeated commit never counts twice"
    );
    // Caller byte counts are never trusted: over-cap content fails the commit.
    let reservation = runtime.reserve_private_state("ag_a", 100).unwrap();
    std::fs::write(a.join("images").join("big.png"), vec![0; 600]).unwrap();
    assert_eq!(
        runtime
            .commit_private_state_reservation("ag_a", &reservation.reservation_id)
            .unwrap_err()
            .code,
        "private_state_agent_quota_exceeded"
    );
    std::fs::remove_file(a.join("images").join("big.png")).unwrap();

    // Cross-Agent: B cannot commit, release or see A's reservation.
    let reservation = runtime.reserve_private_state("ag_a", 100).unwrap();
    assert_eq!(
        runtime
            .commit_private_state_reservation("ag_b", &reservation.reservation_id)
            .unwrap_err()
            .code,
        "private_state_reservation_expired"
    );
    assert!(
        !runtime
            .release_private_state_reservation("ag_b", &reservation.reservation_id)
            .unwrap(),
        "a foreign release never touches the reservation"
    );
    assert!(runtime
        .private_state_report("ag_b")
        .unwrap()
        .usage
        .unwrap()
        .reservation_ids
        .is_empty());
    assert_eq!(
        runtime
            .private_state_report("ag_a")
            .unwrap()
            .usage
            .unwrap()
            .reservation_ids,
        vec![reservation.reservation_id.clone()]
    );
    assert!(runtime
        .release_private_state_reservation("ag_a", &reservation.reservation_id)
        .unwrap());
    assert!(!runtime
        .release_private_state_reservation("ag_a", &reservation.reservation_id)
        .unwrap());

    // Global cap: A holds 450; B may not push the machine beyond 1300.
    std::fs::write(b.join("blob"), vec![0; 700]).unwrap();
    assert_eq!(
        runtime.reserve_private_state("ag_b", 200).unwrap_err().code,
        "private_state_global_quota_exceeded"
    );
    assert!(runtime.reserve_private_state("ag_b", 100).is_ok());
    let b_usage = runtime.private_state_report("ag_b").unwrap().usage.unwrap();
    assert_eq!(b_usage.used_bytes, 700);
    assert_eq!(
        b_usage.available_bytes, 50,
        "min(agent headroom 200, global 50) after the open 100"
    );

    // Crash recount: a replacement Controller measures the disk, not memory.
    drop(runtime);
    let replacement = fixture
        .runtime(fixture.config("srv_a", private_root.clone()))
        .await
        .unwrap();
    replacement
        .inner
        .private_state_ledger
        .lock()
        .unwrap()
        .set_limits(limits);
    let a_usage = replacement
        .private_state_report("ag_a")
        .unwrap()
        .usage
        .unwrap();
    assert_eq!(a_usage.used_bytes, 450);
    assert!(
        a_usage.reservation_ids.is_empty(),
        "reservations do not survive a crash"
    );
    assert_eq!(
        replacement
            .reserve_private_state("ag_b", 200)
            .unwrap_err()
            .code,
        "private_state_global_quota_exceeded"
    );
}

#[tokio::test]
async fn local_api_requires_the_workload_identity_and_scopes_to_the_caller() {
    let fixture = fixture().await;
    let private_root = fixture.state.join("agent-private");
    let runtime = fixture
        .runtime(fixture.config("srv_a", private_root.clone()))
        .await
        .unwrap();
    let credential_a = create(&runtime, &fixture, "ag_a", "repo").await;
    let credential_b = create(&runtime, &fixture, "ag_b", "repo").await;
    let state = crate::local_api::LocalApiState::new(
        url::Url::parse("http://127.0.0.1:1/").unwrap(),
        "workspace".into(),
        "srv_a".into(),
        None,
        Some("operator-secret".into()),
        treer_protocol::BuildInfo {
            version: "test".into(),
            git_commit: "test".into(),
        },
        runtime.clone(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(axum::serve(listener, crate::local_api::router(state)).into_future());
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let agent = |agent_id: &str, credential: &str| {
        [
            (treer_protocol::AGENT_ID_HEADER, agent_id.to_string()),
            (
                treer_protocol::WORKLOAD_CREDENTIAL_HEADER,
                credential.to_string(),
            ),
        ]
    };
    let with = |request: reqwest::RequestBuilder, headers: [(&'static str, String); 2]| {
        headers.into_iter().fold(request, |request, (name, value)| {
            request.header(name, value)
        })
    };

    // Anonymous and operator-only requests are refused: the directory is
    // always selected by an authenticated workload identity.
    let anonymous = client
        .get(format!("{base}/api/private-state"))
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status(), 401);
    let operator = client
        .get(format!("{base}/api/private-state"))
        .header(
            treer_protocol::OPERATOR_CREDENTIAL_HEADER,
            "operator-secret",
        )
        .send()
        .await
        .unwrap();
    assert_eq!(operator.status(), 401);
    assert_eq!(
        operator.json::<serde_json::Value>().await.unwrap()["error"]["code"],
        "workload_identity_required"
    );
    let forged = with(
        client.get(format!("{base}/api/private-state")),
        agent("ag_a", &credential_b),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(
        forged.status(),
        401,
        "another Agent's credential never selects a directory"
    );

    let report = with(
        client.get(format!("{base}/api/private-state")),
        agent("ag_a", &credential_a),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(report.status(), 200);
    let report = report.json::<serde_json::Value>().await.unwrap();
    assert_eq!(report["status"], "available");
    assert_eq!(
        report["feature"],
        treer_protocol::CAPABILITY_AGENT_PRIVATE_STATE
    );
    assert_eq!(report["usage"]["used_bytes"], 0);
    assert!(report.get("path").is_none(), "no path travels over the API");

    let reserved = with(
        client.post(format!("{base}/api/private-state/reservations")),
        agent("ag_a", &credential_a),
    )
    .json(&serde_json::json!({ "bytes": 1024 }))
    .send()
    .await
    .unwrap();
    assert_eq!(reserved.status(), 201);
    let reserved = reserved.json::<serde_json::Value>().await.unwrap();
    let id = reserved["reservation_id"].as_str().unwrap().to_owned();
    assert_eq!(reserved["bytes"], 1024);
    assert!(reserved["expires_at"].is_string());

    let invalid = with(
        client.post(format!("{base}/api/private-state/reservations")),
        agent("ag_a", &credential_a),
    )
    .json(&serde_json::json!({ "bytes": 0 }))
    .send()
    .await
    .unwrap();
    assert_eq!(invalid.status(), 400);

    let foreign_commit = with(
        client.post(format!("{base}/api/private-state/reservations/{id}/commit")),
        agent("ag_b", &credential_b),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(foreign_commit.status(), 409);
    assert_eq!(
        foreign_commit.json::<serde_json::Value>().await.unwrap()["error"]["code"],
        "private_state_reservation_expired"
    );
    let foreign_release = with(
        client.delete(format!("{base}/api/private-state/reservations/{id}")),
        agent("ag_b", &credential_b),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(foreign_release.status(), 200);
    assert_eq!(
        foreign_release.json::<serde_json::Value>().await.unwrap()["released"],
        false
    );

    std::fs::write(
        private_root.join("srv_a").join("ag_a").join("image.png"),
        vec![0; 1000],
    )
    .unwrap();
    let committed = with(
        client.post(format!("{base}/api/private-state/reservations/{id}/commit")),
        agent("ag_a", &credential_a),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(committed.status(), 200);
    assert_eq!(
        committed.json::<serde_json::Value>().await.unwrap()["usage"]["used_bytes"],
        1000
    );
    let unknown = with(
        client.post(format!(
            "{base}/api/private-state/reservations/psr_unknown/commit"
        )),
        agent("ag_a", &credential_a),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(unknown.status(), 409);
    let idempotent = with(
        client.delete(format!("{base}/api/private-state/reservations/psr_unknown")),
        agent("ag_a", &credential_a),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(idempotent.status(), 200);
    server.abort();
}

#[tokio::test]
async fn a_local_fence_never_purges_and_survives_a_tombstone_purge_of_another_agent() {
    let fixture = fixture().await;
    let private_root = fixture.state.join("agent-private");
    let runtime = fixture
        .runtime(fixture.config("srv_a", private_root.clone()))
        .await
        .unwrap();
    // A reservation that never started is fenced (#123). A leftover directory
    // under that ID is NOT private-state deletion evidence: only the Proxy
    // tombstone is. The fence alone keeps files and refuses Create.
    runtime
        .fence_absent_agent_reservation("ag_fenced")
        .await
        .unwrap();
    let fenced = private_root.join("srv_a").join("ag_fenced");
    std::fs::create_dir(&fenced).unwrap();
    std::fs::write(fenced.join("leftover"), b"x").unwrap();
    assert!(runtime.purge_deleted_private_state(&[]).await.is_empty());
    assert!(
        fenced.join("leftover").exists(),
        "a fence alone never purges"
    );
    let denied = runtime
        .create(
            "create-fenced",
            "ag_fenced".into(),
            new_workload_credential(),
            fixture.request("repo"),
        )
        .await
        .unwrap_err();
    assert_eq!(denied.code, "agent_identity_denied");
    assert!(fixture.spawns.lock().unwrap().is_empty());

    // Purging a tombstoned Agent removes its startup entry through the store's
    // atomic write and leaves the fence in place, on disk and in memory.
    create(&runtime, &fixture, "ag_gone", "repo").await;
    runtime.stop("stop", "ag_gone").await.unwrap();
    let results = runtime
        .purge_deleted_private_state(&["ag_gone".into()])
        .await;
    assert_eq!(results[0].1, Ok(true));
    assert!(runtime.get_startup("ag_gone").is_err());
    assert!(runtime.inner.startup_store.is_denied("ag_fenced").unwrap());
    assert!(
        StartupStore::load(fixture.root.join("startup.json"))
            .unwrap()
            .is_denied("ag_fenced")
            .unwrap(),
        "the persisted fence survives the entry removal"
    );
    let denied = runtime
        .create(
            "create-fenced-again",
            "ag_fenced".into(),
            new_workload_credential(),
            fixture.request("repo"),
        )
        .await
        .unwrap_err();
    assert_eq!(denied.code, "agent_identity_denied");
    assert!(fenced.join("leftover").exists());

    // Only the tombstone for the fenced ID itself removes that directory.
    let results = runtime
        .purge_deleted_private_state(&["ag_fenced".into()])
        .await;
    assert_eq!(results[0].1, Ok(true));
    assert!(!fenced.exists());
    assert!(runtime.inner.startup_store.is_denied("ag_fenced").unwrap());
}

#[test]
fn private_state_status_codes_are_fixed_and_include_the_windows_product_rule() {
    assert_eq!(
        PrivateStateStatus::RequiresAdministrator.code(),
        "windows_requires_administrator"
    );
    assert_eq!(
        PrivateStateStatus::Unsafe.code(),
        "agent_private_state_unsafe"
    );
    assert_eq!(
        PrivateStateStatus::InsideCwd.code(),
        "agent_private_state_inside_cwd"
    );
    assert_eq!(
        PrivateStateStatus::Unavailable.code(),
        "agent_private_state_unavailable"
    );
}

/// Hosted and native Windows gates run as administrators: the live token must
/// classify as such, so private state is served there.
#[cfg(windows)]
#[test]
fn windows_gate_runner_is_an_administrator_for_private_state() {
    let class = treer_platform::account_class().unwrap();
    assert_ne!(
        class,
        treer_platform::WindowsAccountClass::Standard,
        "{class:?}"
    );
    // The unsplit-token branch (DuplicateToken on a TOKEN_QUERY|TOKEN_DUPLICATE
    // process token + CheckTokenMembership) must execute without an access
    // error whatever the runner's elevation type; the verdict itself depends
    // on that type (enabled group => true, UAC-limited => deny-only => false).
    let member = treer_platform::process_token_administrators_member().unwrap();
    if class == treer_platform::WindowsAccountClass::Elevated {
        assert!(
            member,
            "an elevated or unsplit administrator token has the group enabled"
        );
    }
}
