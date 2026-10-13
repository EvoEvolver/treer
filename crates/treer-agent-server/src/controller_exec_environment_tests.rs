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
        assert_eq!(response.exit_code,Some(0),"{command} failed: {}",response.stderr);
        assert!(response.stdout.contains(expected),"{command} did not complete its smoke check");
    }
    host.abort();
    drop(runtime);
    std::fs::remove_dir_all(root).unwrap();
}
