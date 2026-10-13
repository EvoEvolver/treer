use super::*;

fn test_root() -> std::path::PathBuf {
    #[cfg(unix)]
    let base = std::path::PathBuf::from("/tmp");
    #[cfg(windows)]
    let base = std::env::temp_dir();
    let root = base.join(format!("trr-ee-{}", Uuid::new_v4().simple()));
    treer_platform::secure_directory(&root).unwrap();
    std::fs::canonicalize(root).unwrap()
}

async fn runtime_for_exec(root: &Path) -> (ControllerRuntime, tokio::task::JoinHandle<()>) {
    #[cfg(unix)]
    let socket = root.join("exec-host.sock");
    #[cfg(windows)]
    let socket = treer_platform::endpoint_name(
        &Uuid::new_v4().to_string(),
        &treer_platform::current_user_sid().unwrap(),
    );
    let mut listener = treer_platform::LocalListener::bind(&socket).unwrap();
    let task = tokio::spawn(async move {
        let _stream = listener.accept().await.unwrap();
        std::future::pending::<()>().await;
    });
    let (host, events) = HostClient::connect(&socket).await.unwrap();
    let (runtime, _) = ControllerRuntime::from_sync(
        host,
        HostResponse::Synced {
            host_epoch: "exec-test".into(),
            host_build: treer_host_protocol::HostBuildInfo {
                version: "fixture".into(),
                git_commit: "fixture".into(),
            },
            processes: vec![],
            replay: vec![],
        },
        events,
        ControllerConfig {
            protected_paths: vec![],
            workspace_id: "fixture-workspace".into(),
            server_id: "fixture-server".into(),
            agent_server_url: "http://127.0.0.1:1".into(),
            proxy_public_url: "https://proxy.invalid/".into(),
            treer_binary: None,
            interface_cache_path: root.join("interfaces.json"),
            startup_store_path: root.join("startup.json"),
            root: root.to_path_buf(),
        },
    )
    .unwrap();
    (runtime, task)
}

#[test]
fn machine_exec_does_not_inherit_credentials_or_inject_empty_identity_variables() {
    let mut child = std::process::Command::new(std::env::current_exe().unwrap());
    child.args([
        "--exact",
        "controller::exec_environment_tests::machine_exec_polluted_environment_fixture",
        "--ignored",
        "--nocapture",
    ]);
    for name in [
        "TREER_MACHINE_TOKEN",
        "TREER_OPERATOR_CREDENTIAL",
        "TREER_ENROLLMENT_KEY",
        "TREER_WORKLOAD_CREDENTIAL",
        "TREER_AGENT_ID",
        "TREER_BIN",
        "AGENT_UI_INSTANCE_ID",
        "CODEX_UI_THREAD_ID",
    ] {
        child.env(name, "synthetic-not-a-real-credential");
    }
    child
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .env("HTTPS_PROXY", "http://127.0.0.1:1");
    let output = child.output().unwrap();
    assert!(
        output.status.success(),
        "fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(windows)]
#[test]
fn windows_machine_exec_identity_boundary() {
    machine_exec_does_not_inherit_credentials_or_inject_empty_identity_variables();
}

#[tokio::test]
#[ignore = "isolated subprocess fixture, invoked by the polluted environment test"]
async fn machine_exec_polluted_environment_fixture() {
    let root = test_root();
    let (runtime, host) = runtime_for_exec(&root).await;
    let response = runtime
        .exec_machine(MachineExecRequest {
            cwd: ".".into(),
            command: std::env::current_exe().unwrap().display().to_string(),
            args: vec![
                "--exact".into(),
                "controller::exec_environment_tests::machine_exec_child_fixture".into(),
                "--ignored".into(),
                "--nocapture".into(),
            ],
            timeout_ms: 30_000,
        })
        .await
        .unwrap();
    assert_eq!(
        response.exit_code,
        Some(0),
        "child environment assertions failed: {} {}",
        response.stdout,
        response.stderr
    );
    for kind in ["agent", "app"] {
        let env = runtime.process_environment(Some((kind, "synthetic-fresh-workload-credential")));
        for name in [
            "TREER_MACHINE_TOKEN",
            "TREER_OPERATOR_CREDENTIAL",
            "TREER_ENROLLMENT_KEY",
        ] {
            assert!(!env.contains_key(name), "{kind} contains {name}");
        }
        assert_eq!(
            env.get("TREER_WORKLOAD_CREDENTIAL").map(String::as_str),
            Some("synthetic-fresh-workload-credential")
        );
        assert_eq!(env.get("TREER_AGENT_ID").map(String::as_str), Some(kind));
    }
    host.abort();
    drop(runtime);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "child of the actual Controller exec path"]
fn machine_exec_child_fixture() {
    for (name, _) in std::env::vars_os() {
        let name = name.to_string_lossy().to_ascii_uppercase();
        assert!(
            !name.starts_with("TREER_")
                && !name.starts_with("AGENT_UI_")
                && !name.starts_with("CODEX_UI_"),
            "unexpected identity variable name {name}"
        );
        assert!(
            !["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NODE_OPTIONS"].contains(&name.as_str()),
            "unexpected ambient control variable {name}"
        );
    }
    #[cfg(windows)]
    for name in [
        "SystemRoot",
        "windir",
        "USERPROFILE",
        "USERNAME",
        "APPDATA",
        "LOCALAPPDATA",
        "ProgramData",
        "ProgramFiles",
        "COMPUTERNAME",
    ] {
        assert!(
            std::env::var_os(name).is_some(),
            "missing Windows baseline {name}"
        );
    }
}

#[cfg(windows)]
#[tokio::test]
async fn windows_machine_exec_node_and_powershell_keep_os_baseline() {
    let root = test_root();
    let (runtime, host) = runtime_for_exec(&root).await;
    for (command,args,expected) in [
        ("node",vec!["-e","const assert=require('node:assert'); for(const k of ['SystemRoot','windir','USERPROFILE','APPDATA','LOCALAPPDATA']) assert.ok(process.env[k],k); assert.ok(!Object.keys(process.env).some(k=>k.toUpperCase().startsWith('TREER_'))); console.log(1+1)"],"2"),
        ("powershell.exe",vec!["-NoProfile","-NonInteractive","-Command","if (!$env:SystemRoot -or !$env:USERPROFILE) { exit 2 }; [System.Security.Cryptography.RandomNumberGenerator]::Create().GetBytes((New-Object byte[] 16)); Write-Output 'powershell-ok'"],"powershell-ok"),
        ("tasklist.exe",vec!["/FI","PID eq 0","/NH"],""),
    ] {
        let response = runtime.exec_machine(MachineExecRequest { cwd:".".into(),command:command.into(),args:args.into_iter().map(str::to_owned).collect(),timeout_ms:30_000 }).await.unwrap();
        assert!(!response.timed_out, "{command} timed out after {}ms: exit={:?} stdout={:?} stderr={:?}", response.duration_ms, response.exit_code, response.stdout, response.stderr);
        assert_eq!(response.exit_code,Some(0),"{command} failed after {}ms: stdout={:?} stderr={:?}",response.duration_ms,response.stdout,response.stderr);
        assert!(response.stdout.contains(expected),"{command} did not complete its smoke check");
    }
    host.abort();
    drop(runtime);
    std::fs::remove_dir_all(root).unwrap();
}

/// Diagnostic only: use synthetic scripts, never print inherited values. The
/// normal Windows smoke above remains the required pass/fail release gate.
#[cfg(windows)]
#[tokio::test]
#[ignore = "read-only native Windows diagnosis, run explicitly with --nocapture"]
async fn windows_powershell_exec_diagnostics() {
    use std::collections::BTreeMap;
    use std::ffi::OsString;

    let root = test_root();
    let (runtime, host) = runtime_for_exec(&root).await;
    let scripts = [
        ("startup", "exit 0"),
        ("version", "Write-Output $PSVersionTable.PSVersion.ToString(); exit 0"),
        ("baseline-presence", "Write-Output ('SystemRoot-present=' + [bool]$env:SystemRoot + ';USERPROFILE-present=' + [bool]$env:USERPROFILE); exit 0"),
        ("rng", "$ErrorActionPreference='Stop'; try { [System.Security.Cryptography.RandomNumberGenerator]::Create().GetBytes((New-Object byte[] 16)); Write-Output 'powershell-ok'; exit 0 } catch { Write-Output ('synthetic-probe-error-type=' + $_.Exception.GetType().FullName + ';hresult=' + $_.Exception.HResult); exit 3 }"),
    ];
    // The first probes use precisely the production Controller exec path.
    for (name, script) in scripts {
        let response = runtime
            .exec_machine(MachineExecRequest {
                cwd: ".".into(),
                command: "powershell.exe".into(),
                args: vec![
                    "-NoProfile".into(),
                    "-NonInteractive".into(),
                    "-Command".into(),
                    script.into(),
                ],
                timeout_ms: 5_000,
            })
            .await
            .unwrap();
        println!(
            "production/{name}: exit={:?} timed_out={} duration_ms={} stdout={:?} stderr={:?}",
            response.exit_code,
            response.timed_out,
            response.duration_ms,
            response.stdout,
            response.stderr
        );
    }
    let baseline = treer_platform::machine_exec_environment();
    println!(
        "allowlisted names={:?}",
        baseline.keys().collect::<Vec<_>>()
    );
    // Keep canonical root validation; vary only the synthetic cwd presented to
    // the direct child. No path values are printed.
    let plain_root = match root.components().next() {
        Some(std::path::Component::Prefix(prefix)) => match prefix.kind() {
            std::path::Prefix::VerbatimDisk(drive) => {
                let mut plain = std::path::PathBuf::from(format!("{}:", char::from(drive)));
                for component in root.components().skip(1) {
                    plain.push(component.as_os_str());
                }
                assert_eq!(std::fs::canonicalize(&plain).unwrap(), root);
                plain
            }
            _ => root.clone(),
        },
        _ => root.clone(),
    };
    for (kind, cwd) in [("verbatim", &root), ("plain-drive", &plain_root)] {
        for (command, args) in [
            (
                "powershell.exe",
                vec![
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    "Write-Output 'powershell-cwd-ok'; exit 0",
                ],
            ),
            ("node", vec!["-e", "console.log('node-cwd-ok')"]),
        ] {
            let mut child = tokio::process::Command::new(command)
                .args(args)
                .current_dir(cwd)
                .env_clear()
                .envs(&baseline)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            let started = Instant::now();
            let wait = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
            let timed_out = wait.is_err();
            let status = match wait {
                Ok(status) => status.unwrap(),
                Err(_) => {
                    child.kill().await.unwrap();
                    child.wait().await.unwrap()
                }
            };
            use tokio::io::AsyncReadExt;
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            child
                .stdout
                .take()
                .unwrap()
                .read_to_end(&mut stdout)
                .await
                .unwrap();
            child
                .stderr
                .take()
                .unwrap()
                .read_to_end(&mut stderr)
                .await
                .unwrap();
            println!("cwd/{kind}/{command}: exit={:?} timed_out={} duration_ms={} stdout={:?} stderr={:?}", status.code(), timed_out, started.elapsed().as_millis(), String::from_utf8_lossy(&stdout), String::from_utf8_lossy(&stderr));
        }
    }
    let extras = [
        "PSModulePath",
        "PUBLIC",
        "PROCESSOR_IDENTIFIER",
        "SESSIONNAME",
    ];
    for extra_count in [0, 1, 4] {
        let mut environment: BTreeMap<OsString, OsString> = baseline.clone();
        for name in &extras[..extra_count] {
            if let Some(value) = std::env::var_os(name) {
                environment.insert((*name).into(), value);
            }
        }
        for pipe in [false, true] {
            for (name, script) in scripts {
                let mut command = tokio::process::Command::new("powershell.exe");
                command
                    .args(["-NoProfile", "-NonInteractive", "-Command", script])
                    .current_dir(&root)
                    .env_clear()
                    .envs(&environment)
                    .stdin(if pipe { Stdio::piped() } else { Stdio::null() })
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .kill_on_drop(true);
                let mut child = command.spawn().unwrap();
                // EOF, not an inherited terminal or any Agent's input stream.
                drop(child.stdin.take());
                let started = Instant::now();
                let wait = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
                let timed_out = wait.is_err();
                let status = match wait {
                    Ok(status) => status.unwrap(),
                    Err(_) => {
                        child.kill().await.unwrap();
                        child.wait().await.unwrap()
                    }
                };
                use tokio::io::AsyncReadExt;
                let mut stdout = Vec::new();
                let mut stderr = Vec::new();
                child
                    .stdout
                    .take()
                    .unwrap()
                    .read_to_end(&mut stdout)
                    .await
                    .unwrap();
                child
                    .stderr
                    .take()
                    .unwrap()
                    .read_to_end(&mut stderr)
                    .await
                    .unwrap();
                println!("direct/{name}/extras={:?}/stdin={}: exit={:?} timed_out={} duration_ms={} stdout={:?} stderr={:?}",
                    &extras[..extra_count], if pipe { "closed-pipe" } else { "nul" }, status.code(), timed_out,
                    started.elapsed().as_millis(), String::from_utf8_lossy(&stdout), String::from_utf8_lossy(&stderr));
            }
        }
    }
    host.abort();
    drop(runtime);
    std::fs::remove_dir_all(root).unwrap();
}
