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
            private_state_root: root.join("agent-private"),
            provider_cli_root: root.join("provider-cli"),
            root: root.to_path_buf(),
        },
    )
    .unwrap();
    // Hosted-image compensation ONLY for this in-process exec unit-test
    // fixture, not Controller production/real-Host paths. GitHub runner images
    // supply a pre-generated module-analysis cache; do not substitute arbitrary
    // paths, broaden the inheritance allowlist or change the fixed PSModulePath.
    #[cfg(windows)]
    {
        *runtime.inner.hosted_test_module_cache.lock().unwrap() = hosted_cache_for_fixture(
            std::env::var_os("GITHUB_ACTIONS").as_deref(),
            std::env::var_os("CI").as_deref(),
            std::env::var_os("PSModuleAnalysisCachePath"),
        );
    }
    (runtime, task)
}

fn hosted_cache_for_fixture(
    github_actions: Option<&std::ffi::OsStr>,
    ci: Option<&std::ffi::OsStr>,
    cache: Option<std::ffi::OsString>,
) -> Option<std::ffi::OsString> {
    (github_actions == Some(std::ffi::OsStr::new("true"))
        && ci == Some(std::ffi::OsStr::new("true")))
    .then_some(cache)
    .flatten()
}

#[test]
fn hosted_cache_compensation_requires_both_ci_guards_and_a_supplied_value() {
    use std::ffi::{OsStr, OsString};
    let yes = Some(OsStr::new("true"));
    for (github, ci) in [
        (None, None),
        (yes, None),
        (None, yes),
        (Some(OsStr::new("false")), yes),
    ] {
        assert!(hosted_cache_for_fixture(github, ci, Some("synthetic-cache".into())).is_none());
    }
    assert!(hosted_cache_for_fixture(yes, yes, None).is_none());
    assert_eq!(
        hosted_cache_for_fixture(yes, yes, Some("synthetic-cache".into())),
        Some(OsString::from("synthetic-cache"))
    );
    assert!(!treer_platform::machine_exec_variable_allowed(
        "PSModuleAnalysisCachePath"
    ));
}

#[cfg(windows)]
#[tokio::test]
async fn windows_machine_exec_cache_compensation_requires_fixture_opt_in() {
    let root = test_root();
    let (runtime, host) = runtime_for_exec(&root).await;
    *runtime.inner.hosted_test_module_cache.lock().unwrap() = None;
    assert!(
        !runtime.machine_exec_environment().keys().any(|key| key
            .to_string_lossy()
            .eq_ignore_ascii_case("PSModuleAnalysisCachePath")),
        "no cache is inherited without fixture opt-in, even on CI"
    );
    host.abort();
    drop(runtime);
    std::fs::remove_dir_all(root).unwrap();
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
        .env("HTTPS_PROXY", "http://127.0.0.1:1")
        .env("PSModulePath", "synthetic-user-writable-module-path");
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
    #[cfg(windows)]
    {
        let root = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap());
        let expected = root
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("Modules");
        assert!(
            std::env::var_os("PSModulePath") == Some(expected.into_os_string()),
            "Controller child module path is not the fixed system directory"
        );
    }
}

#[cfg(windows)]
#[tokio::test]
async fn windows_machine_exec_node_and_powershell_keep_os_baseline() {
    let root = test_root();
    let (runtime, host) = runtime_for_exec(&root).await;
    let environment = runtime.machine_exec_environment();
    let system = PathBuf::from(environment.get(std::ffi::OsStr::new("SystemRoot")).unwrap());
    let modules = system
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("Modules");
    assert!(
        environment.get(std::ffi::OsStr::new("PSModulePath")) == Some(&modules.into_os_string()),
        "initial PSModulePath must equal the fixed system directory"
    );
    for (index, (command,args,expected)) in [
        ("node",vec!["-e","const assert=require('node:assert'); for(const k of ['SystemRoot','windir','USERPROFILE','APPDATA','LOCALAPPDATA']) assert.ok(process.env[k],k); assert.ok(!Object.keys(process.env).some(k=>k.toUpperCase().startsWith('TREER_'))); console.log(1+1)"],"2"),
        ("powershell.exe",vec!["-NoProfile","-NonInteractive","-Command","if (!$PSVersionTable.PSVersion) { exit 2 }; Write-Output 'powershell-version-ok'; exit 0"],"powershell-version-ok"),
        ("powershell.exe",vec!["-NoProfile","-NonInteractive","-Command","if (!$env:SystemRoot -or !$env:USERPROFILE -or !$env:PSModulePath) { exit 2 }; Write-Output 'powershell-baseline-ok'; exit 0"],"powershell-baseline-ok"),
        ("node",vec!["-e","const a=require('node:assert'); const p=require('node:path'); a.equal(process.env.PSModulePath,p.win32.join(process.env.SystemRoot,'System32','WindowsPowerShell','v1.0','Modules')); console.log('node-module-path-equal=True')"],"node-module-path-equal=True"),
        ("powershell.exe",vec!["-NoProfile","-NonInteractive","-Command","$expected=[IO.Path]::Combine($env:SystemRoot,'System32\\WindowsPowerShell\\v1.0\\Modules'); $contains=@($env:PSModulePath -split ';' | Where-Object { [string]::Equals($_,$expected,[StringComparison]::OrdinalIgnoreCase) }).Count -gt 0; Write-Output ('powershell-system-module-present=' + $contains); if (!$contains) { exit 3 }; exit 0"],"powershell-system-module-present=True"),
        ("powershell.exe",vec!["-NoProfile","-NonInteractive","-Command","if (!$env:SystemRoot -or !$env:USERPROFILE) { exit 2 }; [System.Security.Cryptography.RandomNumberGenerator]::Create().GetBytes((New-Object byte[] 16)); Write-Output 'powershell-ok'"],"powershell-ok"),
        ("tasklist.exe",vec!["/FI","PID eq 0","/NH"],""),
    ].into_iter().enumerate() {
        let response = runtime.exec_machine(MachineExecRequest { cwd:".".into(),command:command.into(),args:args.into_iter().map(str::to_owned).collect(),timeout_ms:30_000 }).await.unwrap();
        eprintln!("windows-exec probe={index} duration_ms={} timeout={} exit={:?}", response.duration_ms,response.timed_out,response.exit_code);
        assert!(!response.timed_out, "{command} timed out after {}ms: exit={:?} stdout={:?} stderr={:?}",response.duration_ms,response.exit_code,response.stdout,response.stderr);
        assert_eq!(response.exit_code,Some(0),"{command} failed after {}ms: stdout={:?} stderr={:?}",response.duration_ms,response.stdout,response.stderr);
        assert!(response.stdout.contains(expected),"{command} did not complete its smoke check");
    }
    host.abort();
    drop(runtime);
    std::fs::remove_dir_all(root).unwrap();
}
